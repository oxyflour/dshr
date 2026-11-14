//! 最小化模式匹配器。
//!
//! ADC-v1 的 schema/catalog 只需少量固定形态的正则（`^[A-Za-z0-9._-]+$`、
//! `^[a-z0-9][a-z0-9._-]{1,63}$`、`^https://[^\s]+$` 等），本机离线且不允许新增
//! 外部 crate（`regex` 虽在 lock 中但未被授权），因此实现一个只覆盖这些形态的
//! 匹配器：锚点 `^`/`$`、字面量、`.`、字符类 `[...]`（含范围与 `\d \w \s` 及否定）、
//! 量词 `+ * ? {n} {n,} {n,m}`。不支持分组/选择/回溯引用。
//!
//! 被匹配的字符串来自插件输入，匹配器必须线性/有界且不能 panic；无法识别的模式
//! 返回 `false`（保守拒绝），非法模式不会导致 UB 或 panic。

/// 字符类中的成员。
#[derive(Debug, Clone)]
enum ClassItem {
    Range(char, char),
    Digit,
    Word,
    Space,
}

impl ClassItem {
    fn matches(&self, c: char) -> bool {
        match self {
            ClassItem::Range(lo, hi) => *lo <= c && c <= *hi,
            ClassItem::Digit => c.is_ascii_digit(),
            ClassItem::Word => c.is_ascii_alphanumeric() || c == '_',
            ClassItem::Space => c.is_whitespace(),
        }
    }
}

/// `[...]` 字符类。
#[derive(Debug, Clone)]
struct CharClass {
    negated: bool,
    items: Vec<ClassItem>,
}

impl CharClass {
    fn matches(&self, c: char) -> bool {
        let hit = self.items.iter().any(|item| item.matches(c));
        hit != self.negated
    }
}

/// 单个原子。
#[derive(Debug, Clone)]
enum Atom {
    Char(char),
    Any,
    Class(CharClass),
}

impl Atom {
    fn matches(&self, c: char) -> bool {
        match self {
            Atom::Char(expected) => *expected == c,
            Atom::Any => true,
            Atom::Class(class) => class.matches(c),
        }
    }
}

/// 原子 + 量词。
#[derive(Debug, Clone)]
struct Piece {
    atom: Atom,
    min: usize,
    max: Option<usize>,
}

impl Piece {
    fn at_most(&self, remaining: usize) -> usize {
        match self.max {
            Some(max) => max.min(remaining),
            None => remaining,
        }
    }
}

/// 匹配 `pattern` 是否完整覆盖 `value`。
///
/// 模式按完整匹配处理（无论是否显式写出 `^`/`$`）；非法模式保守返回 `false`。
pub(crate) fn matches_pattern(pattern: &str, value: &str) -> bool {
    let Some(pieces) = parse_pattern(pattern) else {
        return false;
    };
    let chars: Vec<char> = value.chars().collect();
    match_from(&pieces, 0, &chars, 0)
}

fn match_from(pieces: &[Piece], piece_index: usize, value: &[char], value_index: usize) -> bool {
    let Some(piece) = pieces.get(piece_index) else {
        return value_index == value.len();
    };
    let remaining = value.len() - value_index;
    let mut count = 0usize;
    while count < piece.at_most(remaining) && piece.atom.matches(value[value_index + count]) {
        count += 1;
    }
    if count < piece.min {
        return false;
    }
    loop {
        if match_from(pieces, piece_index + 1, value, value_index + count) {
            return true;
        }
        if count == piece.min {
            return false;
        }
        count -= 1;
    }
}

fn parse_pattern(pattern: &str) -> Option<Vec<Piece>> {
    let mut chars = pattern.chars().peekable();
    if chars.peek() == Some(&'^') {
        chars.next();
    }
    let body: Vec<char> = chars.collect();
    let body_len = if body.last() == Some(&'$') {
        body.len() - 1
    } else {
        body.len()
    };
    let mut pieces = Vec::new();
    let mut index = 0usize;
    while index < body_len {
        let (atom, next) = match body[index] {
            '\\' => {
                if index + 1 >= body_len {
                    return None;
                }
                (escape_atom(body[index + 1])?, index + 2)
            }
            '[' => {
                let (class, next) = parse_class(&body, index, body_len)?;
                (Atom::Class(class), next)
            }
            '.' => (Atom::Any, index + 1),
            '(' | ')' | '|' => return None,
            literal => (Atom::Char(literal), index + 1),
        };
        let (min, max, next) = match parse_quantifier(&body, next) {
            Some(((min, max), after)) => (min, max, after),
            None => (1, Some(1), next),
        };
        index = next;
        pieces.push(Piece { atom, min, max });
    }
    Some(pieces)
}

fn escape_atom(c: char) -> Option<Atom> {
    match c {
        'd' => Some(Atom::Class(CharClass {
            negated: false,
            items: vec![ClassItem::Digit],
        })),
        'w' => Some(Atom::Class(CharClass {
            negated: false,
            items: vec![ClassItem::Word],
        })),
        's' => Some(Atom::Class(CharClass {
            negated: false,
            items: vec![ClassItem::Space],
        })),
        'D' => Some(Atom::Class(CharClass {
            negated: true,
            items: vec![ClassItem::Digit],
        })),
        'W' => Some(Atom::Class(CharClass {
            negated: true,
            items: vec![ClassItem::Word],
        })),
        'S' => Some(Atom::Class(CharClass {
            negated: true,
            items: vec![ClassItem::Space],
        })),
        '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' => {
            Some(Atom::Char(c))
        }
        _ => None,
    }
}

/// 解析 `[...]`，`index` 指向 `[`；返回字符类与 `]` 之后的位置。
fn parse_class(body: &[char], index: usize, body_len: usize) -> Option<(CharClass, usize)> {
    let mut cursor = index + 1;
    let mut negated = false;
    if cursor < body_len && body[cursor] == '^' {
        negated = true;
        cursor += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    while cursor < body_len {
        let c = body[cursor];
        if c == ']' && !first {
            return Some((CharClass { negated, items }, cursor + 1));
        }
        first = false;
        if c == '\\' {
            cursor += 1;
            if cursor >= body_len {
                return None;
            }
            match body[cursor] {
                'd' => items.push(ClassItem::Digit),
                'w' => items.push(ClassItem::Word),
                's' => items.push(ClassItem::Space),
                other => items.push(ClassItem::Range(other, other)),
            }
            cursor += 1;
            continue;
        }
        // 范围 a-z：仅当后两个字符分别为 '-' 和普通字符时成立。
        if cursor + 2 < body_len && body[cursor + 1] == '-' && body[cursor + 2] != ']' {
            items.push(ClassItem::Range(c, body[cursor + 2]));
            cursor += 3;
            continue;
        }
        items.push(ClassItem::Range(c, c));
        cursor += 1;
    }
    None
}

/// 解析量词，`index` 指向量词首字符；返回 `(min, max)` 与新位置。
fn parse_quantifier(body: &[char], index: usize) -> Option<((usize, Option<usize>), usize)> {
    match body.get(index)? {
        '+' => Some(((1, None), index + 1)),
        '*' => Some(((0, None), index + 1)),
        '?' => Some(((0, Some(1)), index + 1)),
        '{' => {
            let mut cursor = index + 1;
            let min = parse_number(body, &mut cursor)?;
            if cursor >= body.len() {
                return None;
            }
            let max = if body[cursor] == '}' {
                cursor += 1;
                Some(min)
            } else if body[cursor] == ',' {
                cursor += 1;
                if cursor < body.len() && body[cursor] == '}' {
                    cursor += 1;
                    None
                } else {
                    let max = parse_number(body, &mut cursor)?;
                    if cursor >= body.len() || body[cursor] != '}' {
                        return None;
                    }
                    cursor += 1;
                    Some(max)
                }
            } else {
                return None;
            };
            if max.is_some_and(|max| max < min) {
                return None;
            }
            Some(((min, max), cursor))
        }
        _ => None,
    }
}

fn parse_number(body: &[char], cursor: &mut usize) -> Option<usize> {
    let start = *cursor;
    let mut value = 0usize;
    while *cursor < body.len() && body[*cursor].is_ascii_digit() {
        value = value
            .checked_mul(10)?
            .checked_add(body[*cursor] as usize - '0' as usize)?;
        *cursor += 1;
    }
    if *cursor == start {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::matches_pattern;

    #[test]
    fn adc_slot_pattern_reference_matches() {
        let id = "^[A-Za-z0-9._-]+$";
        assert!(matches_pattern(id, "cad.inspector_1-A"));
        assert!(!matches_pattern(id, "bad id!"));
        assert!(!matches_pattern(id, ""));

        let plugin = "^[a-z0-9][a-z0-9._-]{1,63}$";
        assert!(matches_pattern(plugin, "acme.cad"));
        assert!(!matches_pattern(plugin, "Acme"));
        assert!(!matches_pattern(plugin, "a"));
    }

    #[test]
    fn adc_sec_https_pattern() {
        let https = "^https://[^\\s]+$";
        assert!(matches_pattern(https, "https://example.com/a?b=1"));
        assert!(!matches_pattern(https, "http://example.com"));
        assert!(!matches_pattern(https, "https://exa mple.com"));
        assert!(!matches_pattern(https, "javascript:alert(1)"));
    }
}
