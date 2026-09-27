// Command tlgen генерирует Go-кодек TL (пакет net/tl) из схем в schema/
// (шаг 1 сборки, `just tl`; Архитектура/nuncio - Сборка и структура репозитория.md).
//
// Генерируется не вся схема Telegram, а замыкание от корней из schema/used.txt
// (решение О-8): указанные функции и конструкторы, все типы их полей и
// результатов, все конструкторы этих типов. Служебный слой mtproto.tl
// включается целиком. Конструкторы, помеченные в схеме «parsed manually»
// (rpc_result, message, msg_container, gzip_packed), закомментированы в самой
// схеме и разбираются вручную в пакете session.
//
// Соответствие типов TL и Go:
//
//	int, long, double      -> int32, int64, float64
//	string, bytes          -> string, []byte
//	int128, int256         -> [16]byte, [32]byte
//	Bool, true (в флагах)  -> bool
//	Тип (boxed)            -> интерфейс <Тип>Class, разбор DecodeXxxClass
//	конструктор (bare), %T -> структура значением
//	Vector<T>, vector<T>   -> []T
//	Object, !X             -> tlbin.Object
//	flags.N?T              -> указатель (*int32, *string ...), nil-срез, nil-интерфейс
//	                          или bool для true; поле flags вычисляется при кодировании
package main

import (
	"bufio"
	"bytes"
	"flag"
	"fmt"
	"go/format"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"unicode"
)

type param struct {
	Name     string
	Type     string // выражение типа как в схеме
	FlagVar  string // для flags.N?T — имя поля флагов
	FlagBit  int
	Optional bool
}

type combinator struct {
	Name     string
	ID       uint32
	Params   []param
	Result   string // тип результата (для конструктора — класс)
	Function bool
	Source   string // mtproto.tl или api.tl
}

type schema struct {
	byName  map[string]*combinator   // конструкторы и функции по имени
	classes map[string][]*combinator // класс -> конструкторы
	layer   int
}

var lineRe = regexp.MustCompile(`^([a-zA-Z0-9_.]+)#([0-9a-fA-F]+)\s*(.*?)\s*=\s*([^;]+);\s*$`)

// builtin — типы, которые не порождают код.
var builtin = map[string]bool{
	"int": true, "long": true, "double": true, "string": true, "bytes": true,
	"int128": true, "int256": true, "Bool": true, "true": true, "#": true,
	"Object": true, "Type": true,
}

// skipped — встроенные конструкторы, реализованные в tlbin.
var skipped = map[string]bool{"boolTrue": true, "boolFalse": true, "vector": true}

func parse(path, source string, s *schema) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()
	functions := false
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<20)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if m := regexp.MustCompile(`^//\s*LAYER\s+(\d+)`).FindStringSubmatch(line); m != nil {
			s.layer, _ = strconv.Atoi(m[1])
		}
		if i := strings.Index(line, "//"); i >= 0 {
			line = strings.TrimSpace(line[:i])
		}
		switch line {
		case "":
			continue
		case "---functions---":
			functions = true
			continue
		case "---types---":
			functions = false
			continue
		}
		m := lineRe.FindStringSubmatch(line)
		if m == nil {
			continue // встроенные описания (int ? = Int, vector {t:Type} ...) и строки без #id
		}
		if skipped[m[1]] {
			continue
		}
		id, err := strconv.ParseUint(m[2], 16, 32)
		if err != nil {
			return fmt.Errorf("%s: %s: %w", source, m[1], err)
		}
		c := &combinator{Name: m[1], ID: uint32(id), Result: strings.TrimSpace(m[4]), Function: functions, Source: source}
		for _, tok := range strings.Fields(m[3]) {
			if strings.HasPrefix(tok, "{") {
				continue // обобщённый параметр {X:Type}
			}
			i := strings.IndexByte(tok, ':')
			if i < 0 {
				return fmt.Errorf("%s: %s: непонятный параметр %q", source, c.Name, tok)
			}
			p := param{Name: tok[:i], Type: tok[i+1:]}
			if q := strings.IndexByte(p.Type, '?'); q >= 0 {
				cond := p.Type[:q]
				p.Type = p.Type[q+1:]
				dot := strings.IndexByte(cond, '.')
				if dot < 0 {
					return fmt.Errorf("%s: %s: непонятное условие %q", source, c.Name, cond)
				}
				p.FlagVar = cond[:dot]
				p.FlagBit, err = strconv.Atoi(cond[dot+1:])
				if err != nil {
					return fmt.Errorf("%s: %s: %w", source, c.Name, err)
				}
				p.Optional = true
			}
			c.Params = append(c.Params, p)
		}
		if old, ok := s.byName[c.Name]; ok && old.ID != c.ID {
			return fmt.Errorf("%s: %s: повторное определение с другим id", source, c.Name)
		}
		s.byName[c.Name] = c
		if !functions {
			s.classes[c.Result] = append(s.classes[c.Result], c)
		}
	}
	return sc.Err()
}

// ---------------------------------------------------------------- типы

type texpr struct {
	kind  string // prim, class, bare, vector, object
	name  string // для prim: int/long/...; class: имя класса; bare: имя конструктора
	boxed bool   // для vector
	elem  *texpr
}

func parseType(t string, s *schema) (*texpr, error) {
	switch {
	case t == "#":
		return &texpr{kind: "flags"}, nil
	case strings.HasPrefix(t, "!"), t == "Object":
		return &texpr{kind: "object"}, nil
	case strings.HasPrefix(t, "Vector<") && strings.HasSuffix(t, ">"):
		e, err := parseType(t[7:len(t)-1], s)
		return &texpr{kind: "vector", boxed: true, elem: e}, err
	case strings.HasPrefix(t, "vector<") && strings.HasSuffix(t, ">"):
		e, err := parseType(t[7:len(t)-1], s)
		return &texpr{kind: "vector", boxed: false, elem: e}, err
	case strings.HasPrefix(t, "%"):
		name := t[1:]
		cs := s.classes[name]
		if len(cs) != 1 {
			return nil, fmt.Errorf("голый тип %s: у класса %d конструкторов", t, len(cs))
		}
		return &texpr{kind: "bare", name: cs[0].Name}, nil
	case builtin[t]:
		return &texpr{kind: "prim", name: t}, nil
	}
	last := t
	if i := strings.LastIndexByte(t, '.'); i >= 0 {
		last = t[i+1:]
	}
	if last != "" && unicode.IsUpper(rune(last[0])) {
		if _, ok := s.classes[t]; !ok {
			return nil, fmt.Errorf("неизвестный тип %s", t)
		}
		return &texpr{kind: "class", name: t}, nil
	}
	if c, ok := s.byName[t]; ok && !c.Function {
		return &texpr{kind: "bare", name: t}, nil
	}
	return nil, fmt.Errorf("неизвестный тип %s", t)
}

// ---------------------------------------------------------------- замыкание

func closure(s *schema, roots []string) (map[string]bool, error) {
	incl := map[string]bool{}
	var queue []string
	add := func(name string) {
		if !incl[name] {
			incl[name] = true
			queue = append(queue, name)
		}
	}
	for _, c := range s.byName {
		if c.Source == "mtproto.tl" {
			add(c.Name)
		}
	}
	for _, r := range roots {
		if _, ok := s.byName[r]; !ok {
			return nil, fmt.Errorf("used.txt: %q нет в схеме", r)
		}
		add(r)
	}
	var visit func(t *texpr)
	visit = func(t *texpr) {
		switch t.kind {
		case "class":
			for _, c := range s.classes[t.name] {
				add(c.Name)
			}
		case "bare":
			add(t.name)
		case "vector":
			visit(t.elem)
		}
	}
	for len(queue) > 0 {
		c := s.byName[queue[0]]
		queue = queue[1:]
		types := []string{}
		for _, p := range c.Params {
			types = append(types, p.Type)
		}
		if c.Function {
			types = append(types, c.Result)
		}
		for _, ts := range types {
			if ts == "X" {
				continue
			}
			t, err := parseType(ts, s)
			if err != nil {
				return nil, fmt.Errorf("%s: %w", c.Name, err)
			}
			visit(t)
		}
		if !c.Function {
			for _, other := range s.classes[c.Result] {
				add(other.Name)
			}
		}
	}
	return incl, nil
}

// ---------------------------------------------------------------- имена Go

func goName(s string) string {
	var b strings.Builder
	for _, part := range strings.FieldsFunc(s, func(r rune) bool { return r == '.' || r == '_' }) {
		r := []rune(part)
		r[0] = unicode.ToUpper(r[0])
		b.WriteString(string(r))
	}
	return b.String()
}

func structName(c *combinator) string {
	if c.Function {
		return goName(c.Name) + "Request"
	}
	return goName(c.Name)
}

func className(t string) string { return goName(t) + "Class" }

func goType(t *texpr) string {
	switch t.kind {
	case "prim":
		return map[string]string{"int": "int32", "long": "int64", "double": "float64", "string": "string",
			"bytes": "[]byte", "int128": "[16]byte", "int256": "[32]byte", "Bool": "bool", "true": "bool"}[t.name]
	case "class":
		return className(t.name)
	case "bare":
		return goName(t.name)
	case "vector":
		return "[]" + goType(t.elem)
	case "object":
		return "tlbin.Object"
	}
	panic("goType " + t.kind)
}

// optType — тип Go для необязательного поля.
func optType(t *texpr) string {
	g := goType(t)
	switch {
	case t.kind == "prim" && t.name == "true":
		return "bool"
	case t.kind == "prim" && t.name == "bytes", t.kind == "vector", t.kind == "class", t.kind == "object":
		return g // nil = отсутствует
	default:
		return "*" + g
	}
}

func minSize(t *texpr) int {
	switch t.kind {
	case "prim":
		return map[string]int{"int": 4, "long": 8, "double": 8, "string": 4, "bytes": 4, "int128": 16,
			"int256": 32, "Bool": 4, "true": 0}[t.name]
	case "bare":
		return 1
	default:
		return 4
	}
}

// ---------------------------------------------------------------- кодогенерация

type gen struct {
	s   *schema
	buf bytes.Buffer
	tmp int
}

func (g *gen) p(format string, a ...any) { fmt.Fprintf(&g.buf, format+"\n", a...) }

func (g *gen) v() string {
	g.tmp++
	return fmt.Sprintf("v%d", g.tmp)
}

// encode пишет код кодирования выражения x типа t.
func (g *gen) encode(x string, t *texpr) {
	switch t.kind {
	case "prim":
		switch t.name {
		case "int":
			g.p("e.PutInt32(%s)", x)
		case "long":
			g.p("e.PutInt64(%s)", x)
		case "double":
			g.p("e.PutDouble(%s)", x)
		case "string":
			g.p("e.PutString(%s)", x)
		case "bytes":
			g.p("e.PutBytes(%s)", x)
		case "int128":
			g.p("e.PutInt128(%s)", x)
		case "int256":
			g.p("e.PutInt256(%s)", x)
		case "Bool":
			g.p("e.PutBool(%s)", x)
		}
	case "class", "object":
		g.p("if err := tlbin.EncodeBoxed(e, %s); err != nil { return err }", x)
	case "bare":
		g.p("if err := (&%s).TLEncodeBare(e); err != nil { return err }", x)
	case "vector":
		g.p("e.PutVectorHeader(len(%s), %v)", x, t.boxed)
		i := g.v()
		g.p("for %s := range %s {", i, x)
		g.encode(fmt.Sprintf("%s[%s]", x, i), t.elem)
		g.p("}")
	}
}

// decode пишет код, кладущий значение типа t в переменную dst (уже объявленную).
func (g *gen) decode(dst string, t *texpr) {
	switch t.kind {
	case "prim":
		m := map[string]string{"int": "Int32", "long": "Int64", "double": "Double", "string": "String",
			"bytes": "Bytes", "int128": "Int128", "int256": "Int256", "Bool": "Bool"}[t.name]
		g.p("if %s, err = d.%s(); err != nil { return err }", dst, m)
	case "class":
		g.p("if %s, err = Decode%s(d); err != nil { return err }", dst, className(t.name))
	case "object":
		g.p("if %s, err = DecodeObject(d); err != nil { return err }", dst)
	case "bare":
		g.p("if err = (&%s).TLDecodeBare(d); err != nil { return err }", dst)
	case "vector":
		n := g.v()
		g.p("%s, err := d.VectorHeader(%v, %d)", n, t.boxed, minSize(t.elem))
		g.p("if err != nil { return err }")
		g.p("%s = make(%s, %s)", dst, goType(t), n)
		i := g.v()
		g.p("for %s := range %s {", i, dst)
		g.decode(fmt.Sprintf("%s[%s]", dst, i), t.elem)
		g.p("}")
	}
}

func (g *gen) combinator(c *combinator) error {
	name := structName(c)
	g.p("// %s — %s#%08x (%s).", name, c.Name, c.ID, c.Source)
	g.p("type %s struct {", name)
	types := make([]*texpr, len(c.Params))
	for i, p := range c.Params {
		t, err := parseType(p.Type, g.s)
		if err != nil {
			return fmt.Errorf("%s.%s: %w", c.Name, p.Name, err)
		}
		types[i] = t
		if t.kind == "flags" {
			continue
		}
		if p.Optional {
			g.p("\t%s %s // %s.%d", goName(p.Name), optType(t), p.FlagVar, p.FlagBit)
		} else {
			g.p("\t%s %s", goName(p.Name), goType(t))
		}
	}
	g.p("}")
	g.p("")
	g.p("func (*%s) TLID() uint32 { return 0x%08x }", name, c.ID)
	g.p("func (*%s) TLName() string { return %q }", name, c.Name)
	if !c.Function {
		g.p("func (*%s) is%s() {}", name, className(c.Result))
	}

	// Кодирование.
	g.p("func (o *%s) TLEncodeBare(e *tlbin.Encoder) error {", name)
	for i, p := range c.Params {
		t := types[i]
		f := "o." + goName(p.Name)
		if t.kind == "flags" {
			fv := "flags_" + p.Name
			g.p("var %s uint32", fv)
			for j, q := range c.Params {
				if !q.Optional || q.FlagVar != p.Name {
					continue
				}
				qf := "o." + goName(q.Name)
				if types[j].kind == "prim" && types[j].name == "true" {
					g.p("if %s { %s |= 1 << %d }", qf, fv, q.FlagBit)
				} else {
					g.p("if %s != nil { %s |= 1 << %d }", qf, fv, q.FlagBit)
				}
			}
			// Поля с общим битом (например, три поля lang_pack в config) обязаны
			// задаваться вместе: иначе бит поднят, а часть полей не записана.
			groups := map[int][]string{}
			for j, q := range c.Params {
				if q.Optional && q.FlagVar == p.Name && !(types[j].kind == "prim" && types[j].name == "true") {
					groups[q.FlagBit] = append(groups[q.FlagBit], "o."+goName(q.Name))
				}
			}
			bits := make([]int, 0, len(groups))
			for b := range groups {
				bits = append(bits, b)
			}
			sort.Ints(bits)
			for _, b := range bits {
				fs := groups[b]
				if len(fs) < 2 {
					continue
				}
				conds := make([]string, len(fs))
				for k, f := range fs {
					conds[k] = f + " != nil"
				}
				g.p("if n := btoi(%s); n != 0 && n != %d {", strings.Join(conds, ") + btoi("), len(fs))
				g.p("return fmt.Errorf(\"tl: %s: поля с общим флагом %s.%d задаются только вместе\")", c.Name, p.Name, b)
				g.p("}")
			}
			g.p("e.PutUint32(%s)", fv)
			continue
		}
		if p.Optional {
			if t.kind == "prim" && t.name == "true" {
				continue
			}
			g.p("if %s != nil {", f)
			if strings.HasPrefix(optType(t), "*") {
				g.encode("(*"+f+")", t)
			} else {
				g.encode(f, t)
			}
			g.p("}")
			continue
		}
		g.encode(f, t)
	}
	g.p("return nil")
	g.p("}")

	// Разбор.
	g.p("func (o *%s) TLDecodeBare(d *tlbin.Decoder) error {", name)
	g.p("var err error")
	g.p("_ = err")
	for i, p := range c.Params {
		t := types[i]
		f := "o." + goName(p.Name)
		if t.kind == "flags" {
			g.p("var flags_%s uint32", p.Name)
			g.p("if flags_%s, err = d.Uint32(); err != nil { return err }", p.Name)
			continue
		}
		if p.Optional {
			cond := fmt.Sprintf("flags_%s&(1<<%d) != 0", p.FlagVar, p.FlagBit)
			if t.kind == "prim" && t.name == "true" {
				g.p("%s = %s", f, cond)
				continue
			}
			g.p("if %s {", cond)
			if strings.HasPrefix(optType(t), "*") {
				tmp := g.v()
				g.p("var %s %s", tmp, goType(t))
				g.decode(tmp, t)
				g.p("%s = &%s", f, tmp)
			} else {
				g.decode(f, t)
			}
			g.p("}")
			continue
		}
		g.decode(f, t)
	}
	g.p("return nil")
	g.p("}")

	// Результат функции.
	if c.Function {
		g.p("// DecodeResult разбирает ответ на %s (%s).", c.Name, c.Result)
		g.p("func (o *%s) DecodeResult(d *tlbin.Decoder) (any, error) {", name)
		if c.Result == "X" {
			// Обёртка вида invokeWithLayer: результат — результат вложенного запроса.
			var q string
			for _, p := range c.Params {
				if strings.HasPrefix(p.Type, "!") {
					q = "o." + goName(p.Name)
				}
			}
			g.p("r, ok := %s.(tlbin.Request)", q)
			g.p("if !ok { return nil, fmt.Errorf(\"tl: %s: вложенный запрос %%T без DecodeResult\", %s) }", c.Name, q)
			g.p("return r.DecodeResult(d)")
		} else {
			t, err := parseType(c.Result, g.s)
			if err != nil {
				return fmt.Errorf("%s: результат: %w", c.Name, err)
			}
			g.p("var err error")
			g.p("_ = err")
			g.p("var res %s", goType(t))
			g.p("if err := func() error {")
			g.decode("res", t)
			g.p("return nil")
			g.p("}(); err != nil { return nil, err }")
			g.p("return res, nil")
		}
		g.p("}")
	}
	g.p("")
	return nil
}

func run(schemaDir, outDir string) error {
	s := &schema{byName: map[string]*combinator{}, classes: map[string][]*combinator{}}
	for _, f := range []string{"mtproto.tl", "api.tl"} {
		if err := parse(filepath.Join(schemaDir, f), f, s); err != nil {
			return err
		}
	}
	if s.layer == 0 {
		return fmt.Errorf("в api.tl нет строки // LAYER N")
	}
	var roots []string
	usedData, err := os.ReadFile(filepath.Join(schemaDir, "used.txt"))
	if err != nil {
		return err
	}
	for _, l := range strings.Split(string(usedData), "\n") {
		l = strings.TrimSpace(l)
		if l != "" && !strings.HasPrefix(l, "#") {
			roots = append(roots, l)
		}
	}
	incl, err := closure(s, roots)
	if err != nil {
		return err
	}
	var names []string
	for n := range incl {
		names = append(names, n)
	}
	sort.Strings(names)

	// Проверка коллизий имён Go и идентификаторов.
	goNames, ids := map[string]string{}, map[uint32]string{}
	classSet := map[string]bool{}
	for _, n := range names {
		c := s.byName[n]
		if prev, ok := goNames[structName(c)]; ok {
			return fmt.Errorf("коллизия имён Go: %s и %s -> %s", prev, n, structName(c))
		}
		goNames[structName(c)] = n
		if prev, ok := ids[c.ID]; ok {
			return fmt.Errorf("коллизия id %#08x: %s и %s", c.ID, prev, n)
		}
		ids[c.ID] = n
		if !c.Function {
			classSet[c.Result] = true
		}
	}
	var classes []string
	for c := range classSet {
		if _, ok := goNames[className(c)]; ok {
			return fmt.Errorf("коллизия имени класса %s", className(c))
		}
		classes = append(classes, c)
	}
	sort.Strings(classes)

	g := &gen{s: s}
	g.p("// Code generated by tlgen. DO NOT EDIT.")
	g.p("// Источник: schema/mtproto.tl, schema/api.tl (слой %d), корни — schema/used.txt.", s.layer)
	g.p("")
	g.p("package tl")
	g.p("")
	g.p("import (")
	g.p("\t\"fmt\"")
	g.p("")
	g.p("\t\"nuncio/net/tlbin\"")
	g.p(")")
	g.p("")
	g.p("var _ = fmt.Errorf")
	g.p("")
	g.p("func btoi(b bool) int { if b { return 1 }; return 0 }")
	g.p("")
	g.p("// Layer — слой API, из которого сгенерирован кодек (invokeWithLayer).")
	g.p("const Layer = %d", s.layer)
	g.p("")
	for _, cl := range classes {
		cn := className(cl)
		g.p("// %s — тип TL %s.", cn, cl)
		g.p("type %s interface {", cn)
		g.p("\ttlbin.Object")
		g.p("\tis%s()", cn)
		g.p("}")
		g.p("")
		g.p("// Decode%s разбирает boxed-значение типа %s.", cn, cl)
		g.p("func Decode%s(d *tlbin.Decoder) (%s, error) {", cn, cn)
		g.p("if err := d.Enter(); err != nil { return nil, err }")
		g.p("defer d.Leave()")
		g.p("id, err := d.ID()")
		g.p("if err != nil { return nil, err }")
		g.p("var v %s", cn)
		g.p("switch id {")
		for _, c := range s.classes[cl] {
			if incl[c.Name] {
				g.p("case 0x%08x: v = new(%s)", c.ID, structName(c))
			}
		}
		g.p("default: return nil, &tlbin.UnknownConstructorError{ID: id, Type: %q}", cl)
		g.p("}")
		g.p("if err := v.TLDecodeBare(d); err != nil { return nil, err }")
		g.p("return v, nil")
		g.p("}")
		g.p("")
	}
	for _, n := range names {
		if err := g.combinator(s.byName[n]); err != nil {
			return err
		}
	}
	// Реестры.
	g.p("// constructors — все сгенерированные конструкторы (для разбора Object).")
	g.p("var constructors = map[uint32]func() tlbin.Object{")
	for _, n := range names {
		if c := s.byName[n]; !c.Function {
			g.p("0x%08x: func() tlbin.Object { return new(%s) },", c.ID, structName(c))
		}
	}
	g.p("}")
	g.p("")
	g.p("// functions — все сгенерированные функции (для разбора запросов, например поддельным сервером в тестах).")
	g.p("var functions = map[uint32]func() tlbin.Object{")
	for _, n := range names {
		if c := s.byName[n]; c.Function {
			g.p("0x%08x: func() tlbin.Object { return new(%s) },", c.ID, structName(c))
		}
	}
	g.p("}")
	g.p("")
	g.p("// NewObject создаёт пустой объект по идентификатору (конструктор или функция); nil, если неизвестен.")
	g.p("func NewObject(id uint32) tlbin.Object {")
	g.p("if f, ok := constructors[id]; ok { return f() }")
	g.p("if f, ok := functions[id]; ok { return f() }")
	g.p("return nil")
	g.p("}")
	g.p("")
	g.p("// DecodeObject разбирает boxed-объект любого сгенерированного типа (Object, !X).")
	g.p("func DecodeObject(d *tlbin.Decoder) (tlbin.Object, error) {")
	g.p("if err := d.Enter(); err != nil { return nil, err }")
	g.p("defer d.Leave()")
	g.p("id, err := d.ID()")
	g.p("if err != nil { return nil, err }")
	g.p("v := NewObject(id)")
	g.p("if v == nil { return nil, &tlbin.UnknownConstructorError{ID: id, Type: \"Object\"} }")
	g.p("if err := v.TLDecodeBare(d); err != nil { return nil, err }")
	g.p("return v, nil")
	g.p("}")
	g.p("")
	g.p("// ClassConstructors — идентификаторы конструкторов каждого класса (для тестов).")
	g.p("var ClassConstructors = map[string][]uint32{")
	for _, cl := range classes {
		var list []string
		for _, c := range s.classes[cl] {
			if incl[c.Name] {
				list = append(list, fmt.Sprintf("0x%08x", c.ID))
			}
		}
		g.p("%q: {%s},", className(cl), strings.Join(list, ", "))
	}
	g.p("}")

	src, err := format.Source(g.buf.Bytes())
	if err != nil {
		os.WriteFile(filepath.Join(outDir, "generated.go.broken"), g.buf.Bytes(), 0o644)
		return fmt.Errorf("gofmt: %w (сырой вывод в generated.go.broken)", err)
	}
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(outDir, "generated.go"), src, 0o644); err != nil {
		return err
	}
	nf, nc := 0, 0
	for _, n := range names {
		if s.byName[n].Function {
			nf++
		} else {
			nc++
		}
	}
	fmt.Printf("tlgen: слой %d, конструкторов %d, функций %d, классов %d -> %s\n", s.layer, nc, nf, len(classes),
		filepath.Join(outDir, "generated.go"))
	return nil
}

func main() {
	schemaDir := flag.String("schema", "../../schema", "каталог со схемами .tl и used.txt")
	outDir := flag.String("out", "../../net/tl", "куда писать сгенерированный код")
	flag.Parse()
	if err := run(*schemaDir, *outDir); err != nil {
		fmt.Fprintln(os.Stderr, "tlgen:", err)
		os.Exit(1)
	}
}
