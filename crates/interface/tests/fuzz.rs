//! Header parsing reads untrusted text: whatever arrives, `parse_header` returns a model or a refusal that
//! points inside the text, and never panics. Deterministic pseudo-random inputs (no extra dependency).

use judge_interface::{Interface, MAX_FUNCTIONS, parse_header};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const WORDS: [&str; 44] = [
    "int",
    "long",
    "double",
    "bool",
    "void",
    "const",
    "std",
    "::",
    "string",
    "vector",
    "pair",
    "<",
    ">",
    ">>",
    ",",
    "(",
    ")",
    ";",
    "&",
    "&&",
    "*",
    "[",
    "]",
    "{",
    "}",
    "=",
    "...",
    "->",
    "extern",
    "\"C\"",
    "ask",
    "solve",
    "x",
    "using namespace std;",
    "#include <vector>\n",
    "#ifndef G\n#define G\n",
    "#endif\n",
    "#define A 1\n",
    "//\n",
    "/*",
    "*/",
    "\n",
    "\r\n",
    "qhhoj_a",
];

const VALID: &str = "#ifndef G\n#define G\n#include <vector>\nusing namespace std;\n\
    int ask(int i);\nvoid note(const vector<pair<int, long long>>& v, string s);\n\
    const vector<vector<int>>& view();\nvector<int> solve(int n, double d, bool b);\n#endif\n";

fn token_soup(rng: &mut Rng) -> String {
    let count = rng.below(40);
    let mut text = String::new();
    for _ in 0..count {
        text += rng.pick(&WORDS);
        text.push(if rng.below(4) == 0 { '\n' } else { ' ' });
    }
    text
}

fn mutated_valid_header(rng: &mut Rng) -> String {
    let mut chars: Vec<char> = VALID.chars().collect();
    for _ in 0..1 + rng.below(6) {
        let at = rng.below(chars.len());
        match rng.below(4) {
            0 => {
                chars.remove(at);
            }
            1 => chars.insert(
                at,
                rng.pick(&["ồ", "\0", "\\", "#", "<", ">", "\"", "/", "*", "\r", "é"])
                    .chars()
                    .next()
                    .unwrap(),
            ),
            2 => chars.truncate(at),
            _ => {
                let other = rng.below(chars.len());
                chars.swap(at, other);
            }
        }
        if chars.is_empty() {
            break;
        }
    }
    chars.into_iter().collect()
}

fn random_unicode(rng: &mut Rng) -> String {
    (0..rng.below(80)).filter_map(|_| char::from_u32(rng.next() as u32 % 0x3000)).collect()
}

fn check_outcome(text: &str) {
    match parse_header("fuzz.h", text) {
        Ok(interface) => check_model(&interface),
        Err(refusal) => {
            let at = refusal.at.expect("a header refusal has a position");
            let lines = text.matches('\n').count() as u32 + 1;
            assert!(at.line >= 1 && at.line <= lines && at.column >= 1, "{at:?} outside {text:?}");
            assert!(refusal.to_string().starts_with(&format!("fuzz.h:{}:{}:", at.line, at.column)));
            assert!(!refusal.construct.contains('\n'), "the construct is one line: {:?}", refusal.construct);
        }
    }
}

fn check_model(interface: &Interface) {
    assert!(interface.functions.len() <= MAX_FUNCTIONS);
    for (index, function) in interface.functions.iter().enumerate() {
        assert_eq!(usize::from(function.id), index);
        let twins = interface.functions.iter().filter(|other| other.name == function.name).count();
        assert_eq!(twins, 1, "names are unique");
    }
}

#[test]
fn random_text_is_accepted_or_refused_with_a_position_and_never_panics() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..4000 {
        check_outcome(&token_soup(&mut rng));
        check_outcome(&mutated_valid_header(&mut rng));
        check_outcome(&random_unicode(&mut rng));
    }
}

#[test]
fn the_unmutated_sample_header_is_accepted() {
    let interface = parse_header("fuzz.h", VALID).unwrap();
    assert_eq!(interface.functions.len(), 4);
}

#[test]
fn every_prefix_of_a_valid_header_is_accepted_or_refused() {
    for end in (0..=VALID.len()).filter(|end| VALID.is_char_boundary(*end)) {
        check_outcome(&VALID[..end]);
    }
}
