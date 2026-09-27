//! Отображение доменных типов в protobuf `client.proto` и обратно.

use domain::model::{
    AuthState, AuthStep, CodeInfo, Command, CommandResult, ConnState, Country, CountryCode, ErrorInfo, ErrorKind,
    Event, Payload, ProxyKind, ProxySettings, StartupErrorKind, User,
};

use crate::proto::{self, command::Body as C, event::Body as E};

pub fn proxy_to(p: &ProxySettings) -> proto::ProxySettings {
    let kind = match p.kind {
        ProxyKind::None => proto::proxy_settings::Kind::None,
        ProxyKind::Socks5 => proto::proxy_settings::Kind::Socks5,
        ProxyKind::MtProxy => proto::proxy_settings::Kind::Mtproxy,
    };
    proto::ProxySettings {
        kind: kind as i32,
        addr: p.addr.clone(),
        user: p.user.clone(),
        password: p.password.clone(),
        secret: p.secret.clone(),
    }
}

pub fn command_to(id: u64, cmd: Command) -> proto::Command {
    let body = match cmd {
        Command::Ping(message) => C::Ping(proto::Ping { message }),
        Command::RequestQr => C::RequestQr(proto::RequestQr {}),
        Command::SendCode { phone } => C::SendCode(proto::SendCode { phone }),
        Command::ResendCode => C::ResendCode(proto::ResendCode {}),
        Command::CancelCode => C::CancelCode(proto::CancelCode {}),
        Command::SignIn { code } => C::SignIn(proto::SignIn { code }),
        Command::CheckPassword { password } => C::CheckPassword(proto::CheckPassword { password }),
        Command::SignUp { first_name, last_name } => C::SignUp(proto::SignUp { first_name, last_name }),
        Command::LogOut => C::LogOut(proto::LogOut {}),
        Command::SetProxy(p) => C::SetProxy(proto::SetProxy { proxy: Some(proxy_to(&p)) }),
        Command::TestProxy(p) => C::TestProxy(proto::TestProxy { proxy: Some(proxy_to(&p)) }),
        Command::GetCountries { lang } => C::GetCountries(proto::GetCountries { lang_code: lang }),
        Command::RetryStartup => C::RetryStartup(proto::RetryStartup {}),
    };
    proto::Command { id, account_id: 1, body: Some(body) }
}

fn error_from(e: proto::Error) -> ErrorInfo {
    use proto::error::Kind as K;
    let kind = match K::try_from(e.kind).unwrap_or(K::Internal) {
        K::Network => ErrorKind::Network,
        K::Rpc => ErrorKind::Rpc,
        K::Flood => ErrorKind::Flood,
        K::Auth => ErrorKind::Auth,
        K::Cancelled => ErrorKind::Cancelled,
        K::Internal | K::Unspecified => ErrorKind::Internal,
    };
    ErrorInfo { kind, code: e.code, text: e.text, retry_after: e.retry_after }
}

fn country_from(c: proto::Country) -> Country {
    Country {
        iso2: c.iso2,
        name: c.name,
        default_name: c.default_name,
        codes: c
            .codes
            .into_iter()
            .map(|k| CountryCode { code: k.code, prefixes: k.prefixes, patterns: k.patterns })
            .collect(),
    }
}

fn auth_from(a: proto::AuthState) -> AuthState {
    use proto::auth_state::Step as S;
    let step = match S::try_from(a.step).unwrap_or(S::Unspecified) {
        S::Unspecified => AuthStep::Unknown,
        S::Qr => AuthStep::Qr,
        S::Code => AuthStep::Code,
        S::Password => AuthStep::Password,
        S::SignUp => AuthStep::SignUp,
        S::Ready => AuthStep::Ready,
        S::LoggedOut => AuthStep::LoggedOut,
    };
    AuthState {
        step,
        qr_url: a.qr_url,
        qr_expires_unix: a.qr_expires_unix,
        code: a
            .code
            .map(|c| CodeInfo { kind: c.kind, length: c.length, next_kind: c.next_kind, timeout: c.timeout })
            .unwrap_or_default(),
        password_hint: a.password_hint,
        has_recovery: a.has_recovery,
        terms: a.terms,
        user: a.user.map(|u| User { id: u.id, first_name: u.first_name, last_name: u.last_name, username: u.username }),
        reason: a.reason,
    }
}

/// Событие Go → доменное событие (`None` — пустое или неизвестное событие).
pub fn event_from(ev: proto::Event) -> Option<Event> {
    Some(match ev.body? {
        E::Result(r) => {
            use proto::command_result::Payload as P;
            let payload = match r.payload {
                None => Payload::None,
                Some(P::Pong(p)) => Payload::Pong { message: p.message, core_version: p.core_version },
                Some(P::Countries(c)) => Payload::Countries(c.countries.into_iter().map(country_from).collect()),
                Some(P::ProxyCheck(p)) => Payload::ProxyCheck { rtt_ms: p.rtt_ms },
            };
            Event::Result(CommandResult { cmd_id: r.cmd_id, error: r.error.map(error_from), payload })
        }
        E::Connection(c) => {
            use proto::connection_state::State as S;
            Event::Connection(match S::try_from(c.state).unwrap_or(S::Unspecified) {
                S::Connecting | S::Unspecified => ConnState::Connecting,
                S::Connected => ConnState::Connected,
                S::WaitingNetwork => ConnState::WaitingNetwork,
            })
        }
        E::Auth(a) => Event::Auth(auth_from(a)),
        E::Startup(s) => {
            use proto::startup_error::Kind as K;
            let kind = match K::try_from(s.kind).unwrap_or(K::Other) {
                K::KeyringUnavailable => StartupErrorKind::KeyringUnavailable,
                K::KeyringDismissed => StartupErrorKind::KeyringDismissed,
                K::Config => StartupErrorKind::Config,
                K::Other | K::Unspecified => StartupErrorKind::Other,
            };
            Event::Startup { kind, text: s.text }
        }
        E::Fatal(f) => Event::Fatal(f.text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn every_command_maps_and_survives_encoding() {
        let p = ProxySettings { kind: ProxyKind::MtProxy, addr: "a:1".into(), secret: "dd00".into(), ..Default::default() };
        for cmd in [
            Command::Ping("x".into()),
            Command::RequestQr,
            Command::SendCode { phone: "+7".into() },
            Command::ResendCode,
            Command::CancelCode,
            Command::SignIn { code: "1".into() },
            Command::CheckPassword { password: "p".into() },
            Command::SignUp { first_name: "a".into(), last_name: "b".into() },
            Command::LogOut,
            Command::SetProxy(p.clone()),
            Command::TestProxy(p.clone()),
            Command::GetCountries { lang: "ru".into() },
            Command::RetryStartup,
        ] {
            let pc = command_to(9, cmd.clone());
            assert_eq!(pc.id, 9);
            let back = proto::Command::decode(pc.encode_to_vec().as_slice()).unwrap();
            assert!(back.body.is_some(), "{cmd:?}");
        }
        assert_eq!(proxy_to(&p).kind, proto::proxy_settings::Kind::Mtproxy as i32);
    }

    #[test]
    fn events_map() {
        let ev = proto::Event {
            body: Some(E::Auth(proto::AuthState {
                step: proto::auth_state::Step::Code as i32,
                code: Some(proto::CodeInfo { kind: "app".into(), length: 5, next_kind: "sms".into(), timeout: 30 }),
                ..Default::default()
            })),
        };
        match event_from(ev) {
            Some(Event::Auth(a)) => assert_eq!((a.step, a.code.length), (AuthStep::Code, 5)),
            other => panic!("{other:?}"),
        }
        let ev = proto::Event {
            body: Some(E::Result(proto::CommandResult {
                cmd_id: 3,
                error: Some(proto::Error { kind: proto::error::Kind::Flood as i32, code: 420, text: "FLOOD_WAIT".into(),
                    retry_after: 42 }),
                payload: None,
            })),
        };
        match event_from(ev) {
            Some(Event::Result(r)) => assert_eq!(r.error.unwrap().kind, ErrorKind::Flood),
            other => panic!("{other:?}"),
        }
        assert_eq!(event_from(proto::Event { body: None }), None);
    }
}
