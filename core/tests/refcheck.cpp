// Сверка с эталоном: построчный протокол для tests/reference.py.
// Байты — в hex, целые — десятичные. Ответ — одна строка ("ERR <код>" при ошибке).
//
//   ige_enc K IV DATA | ige_dec K IV DATA            -> DATA
//   keyid AUTHKEY                                    -> ID
//   env_seal X AUTHKEY SALT SESS MSGID SEQ BODY PAD  -> WIRE
//   env_open X AUTHKEY WIRE                          -> SALT SESS MSGID SEQ BODY
//   rsa_pad FP DATA PAD TEMPKEY                      -> ENC (ERR 6 — повторить попытку)
//   tmp_key NEWNONCE SERVERNONCE                     -> KEY IV
//   enc_dh NEWNONCE SERVERNONCE INNER PAD            -> ENC
//   dec_dh NEWNONCE SERVERNONCE ENC                  -> INNER
//   dh NEWNONCE SERVERNONCE P G GA B                 -> GB AUTHKEY NH1 NH2 NH3 AUX
//   srp PW S1 S2 G P GB A                            -> A M1
//   obfs TRANSPORT DC SECRET|- INITRAND PAYLOAD      -> INIT ENCPAYLOAD
//   pq PQ                                            -> P Q
//   known_prime                                      -> встроенный dh_prime
#include "internal.h"

#include <cstdio>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

namespace {

using Bytes = std::vector<uint8_t>;

Bytes unhex(const std::string &s) {
    if (s == "-") return {};
    Bytes out;
    for (size_t i = 0; i + 1 < s.size(); i += 2) out.push_back(static_cast<uint8_t>(std::stoul(s.substr(i, 2), nullptr, 16)));
    return out;
}

std::string hex(const uint8_t *p, size_t n) {
    static const char *d = "0123456789abcdef";
    std::string s;
    s.reserve(2 * n);
    for (size_t i = 0; i < n; ++i) {
        s.push_back(d[p[i] >> 4]);
        s.push_back(d[p[i] & 15]);
    }
    return n == 0 ? "-" : s;
}
std::string hex(const Bytes &b) { return hex(b.data(), b.size()); }
std::string take(tg_buf b) {
    std::string s = hex(b.ptr, b.len);
    tg_buf_free(b);
    return s;
}
std::string err(tg_status st) { return "ERR " + std::to_string(static_cast<int>(st)); }

std::string run(const std::string &line) {
    std::istringstream in(line);
    std::string cmd;
    in >> cmd;
    std::vector<std::string> a;
    for (std::string t; in >> t;) a.push_back(t);

    if (cmd == "ige_enc" || cmd == "ige_dec") {
        const Bytes k = unhex(a[0]), iv = unhex(a[1]), d = unhex(a[2]);
        Bytes out(d.size());
        const bool ok = cmd == "ige_enc" ? tg::aes_ige_encrypt(k.data(), k.size(), iv.data(), d.data(), out.data(), d.size())
                                         : tg::aes_ige_decrypt(k.data(), k.size(), iv.data(), d.data(), out.data(), d.size());
        return ok ? hex(out) : "ERR";
    }
    if (cmd == "keyid") {
        tg_key *k = tg::key_from_bytes(unhex(a[0]).data());
        const std::string r = std::to_string(tg_key_id(k));
        tg_key_free(k);
        return r;
    }
    if (cmd == "env_seal") {
        tg_key *k = tg::key_from_bytes(unhex(a[1]).data());
        tg_env_hdr h{std::stoull(a[2]), std::stoull(a[3]), std::stoull(a[4]), static_cast<uint32_t>(std::stoul(a[5]))};
        const Bytes body = unhex(a[6]), pad = unhex(a[7]);
        tg_buf out{};
        const tg_status st = tg::env_seal(k, std::stoi(a[0]), &h, body.data(), body.size(), pad.data(), pad.size(), &out);
        tg_key_free(k);
        return st == TG_OK ? take(out) : err(st);
    }
    if (cmd == "env_open") {
        tg_key *k = tg::key_from_bytes(unhex(a[1]).data());
        const Bytes w = unhex(a[2]);
        tg_env_hdr h{};
        tg_buf body{};
        const tg_status st = tg::env_open(k, std::stoi(a[0]), w.data(), w.size(), &h, &body);
        tg_key_free(k);
        if (st != TG_OK) return err(st);
        return std::to_string(h.salt) + " " + std::to_string(h.session_id) + " " + std::to_string(h.msg_id) + " " +
               std::to_string(h.seq_no) + " " + take(body);
    }
    if (cmd == "rsa_pad") {
        const Bytes d = unhex(a[1]), pad = unhex(a[2]), tk = unhex(a[3]);
        tg_buf out{};
        const tg_status st = tg::rsa_pad_once(d.data(), d.size(), std::stoull(a[0], nullptr, 16), pad.data(), tk.data(), &out);
        return st == TG_OK ? take(out) : err(st);
    }
    if (cmd == "tmp_key") {
        uint8_t key[32], iv[32];
        tg::authgen_tmp_key(unhex(a[0]).data(), unhex(a[1]).data(), key, iv);
        return hex(key, 32) + " " + hex(iv, 32);
    }
    if (cmd == "enc_dh" || cmd == "dec_dh" || cmd == "dh") {
        tg_authgen *g = nullptr;
        tg_status st = tg_authgen_new(unhex(a[0]).data(), unhex(a[1]).data(), &g);
        if (st != TG_OK) return err(st);
        std::string r;
        tg_buf out{};
        if (cmd == "enc_dh") {
            const Bytes inner = unhex(a[2]), pad = unhex(a[3]);
            st = tg::authgen_encrypt_dh_with_pad(g, inner.data(), inner.size(), pad.data(), pad.size(), &out);
            r = st == TG_OK ? take(out) : err(st);
        } else if (cmd == "dec_dh") {
            const Bytes enc = unhex(a[2]);
            st = tg_authgen_decrypt_dh(g, enc.data(), enc.size(), &out);
            r = st == TG_OK ? take(out) : err(st);
        } else {
            const Bytes p = unhex(a[2]), ga = unhex(a[4]), b = unhex(a[5]);
            st = tg::authgen_dh_with_b(g, p.data(), p.size(), static_cast<uint32_t>(std::stoul(a[3])), ga.data(),
                                       ga.size(), b.data(), &out);
            if (st != TG_OK) {
                r = err(st);
            } else {
                r = take(out);
                uint8_t nh[16];
                uint64_t aux = 0;
                tg_key *key = nullptr;
                std::string hashes;
                for (int n = 1; n <= 3; ++n) {
                    tg_authgen_nonce_hash(g, n, nh);
                    hashes += " " + hex(nh, 16);
                }
                tg_authgen_aux_hash(g, &aux);
                tg_authgen_finish(g, &key);
                r += " " + hex(tg::key_bytes(key), 256) + hashes + " " + std::to_string(aux);
                tg_key_free(key);
            }
        }
        tg_authgen_free(g);
        return r;
    }
    if (cmd == "srp") {
        const Bytes pw = unhex(a[0]), s1 = unhex(a[1]), s2 = unhex(a[2]), p = unhex(a[4]), gb = unhex(a[5]), av = unhex(a[6]);
        tg_buf A{}, M{};
        const tg_status st = tg::srp_check_with_a(pw.data(), pw.size(), s1.data(), s1.size(), s2.data(), s2.size(),
                                                  static_cast<uint32_t>(std::stoul(a[3])), p.data(), p.size(),
                                                  gb.data(), gb.size(), av.data(), 100000, &A, &M);
        if (st != TG_OK) return err(st);
        const std::string ra = take(A);
        return ra + " " + take(M);
    }
    if (cmd == "obfs") {
        const Bytes secret = unhex(a[2]), init_rnd = unhex(a[3]);
        Bytes payload = unhex(a[4]);
        tg_buf init{};
        tg_obfs *o = nullptr;
        const tg_status st = tg::obfs_new_with_init(std::stoi(a[0]), std::stoi(a[1]), secret.data(), secret.size(),
                                                    init_rnd.data(), &init, &o);
        if (st != TG_OK) return err(st);
        tg_obfs_encrypt(o, payload.data(), payload.size());
        tg_obfs_free(o);
        const std::string ri = take(init);
        return ri + " " + hex(payload);
    }
    if (cmd == "rsa_key") {
        const uint8_t *n = tg::rsa_key_modulus(std::stoull(a[0], nullptr, 16));
        return n == nullptr ? "ERR" : hex(n, 256);
    }
    if (cmd == "known_prime") return hex(tg::dh_known_prime(), 256);
    if (cmd == "pq") {
        uint32_t p = 0, q = 0;
        const tg_status st = tg_pq_factorize(std::stoull(a[0]), &p, &q);
        return st == TG_OK ? std::to_string(p) + " " + std::to_string(q) : err(st);
    }
    return "ERR unknown";
}

}  // namespace

int main() {
    std::ios::sync_with_stdio(false);
    for (std::string line; std::getline(std::cin, line);) {
        if (line.empty()) continue;
        std::cout << run(line) << '\n' << std::flush;
    }
    return 0;
}
