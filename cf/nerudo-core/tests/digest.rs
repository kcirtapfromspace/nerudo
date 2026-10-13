//! The rules digest that seals, allowances and release signatures sign. The values are
//! pinned: a change to `Rules` that moves one would void approvals on live trunks.

use nerudo_core::rules::canonical_json;
use nerudo_core::Rules;
use serde::{Deserialize, Serialize};

/// Fixed values for the slots in the demo and replay fixtures.
const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const NAME: &str = "you";

/// Sections no fixture has: a quorum, allowances, [ci], [decide] and [releases].
const RICH: &str = r#"
[seals.require]
governance = { roles = ["maintainer"], count = 1 }

[allowances]
required = true
roles = ["maintainer"]

[ci]
typecheck = { tsconfig = "tsconfig.json" }

[decide]
provider = "fixed"
fixed = { merge = 0.1, review = 0.87, sign = 0.03 }

[decide.risk]
paths = ["src/**"]

[releases]
require = { roles = ["maintainer"], count = 1 }

[[approvers]]
name = "you"
key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
roles = ["maintainer"]
"#;

fn texts() -> Vec<(&'static str, String)> {
    let fill = |t: &str| t.replace("<your landseq pubkey>", KEY).replace("<your name>", NAME);
    let bench = include_str!("../../fixtures/legacy/bench-landseq.toml");
    let rich = format!("{bench}{RICH}");
    // seals.approval, and an approver with no key yet.
    let gated = format!("{}[[approvers]]\nname = \"dana\"\nroles = [\"maintainer\"]\n", rich.replace("[seals]\n", "[seals]\napproval = [\"src/**\"]\n"));
    vec![
        ("bench-landseq.toml", bench.to_string()),
        ("demo-landseq.toml", fill(include_str!("../../fixtures/legacy/demo-landseq.toml"))),
        ("replay/agents.toml", fill(include_str!("../../fixtures/legacy/replay/agents.toml"))),
        ("replay/hono.toml", fill(include_str!("../../fixtures/legacy/replay/hono.toml"))),
        ("replay/landseq.toml", fill(include_str!("../../fixtures/legacy/replay/landseq.toml"))),
        ("rich", rich),
        ("gated", gated),
    ]
}

#[test]
fn digests_are_pinned() {
    // The canonical digest, then the forms of 46e1d5d, e468cda (to 89781d4) and e2523c3 that
    // still parse back to the same rules. Builds of those commits computed the old values.
    let pinned: [(&str, &[&str]); 7] = [
        (
            "bench-landseq.toml",
            &[
                "e226d03fef86ec9f2099675d089ac2de3aa57fded0dbd9c3a6d89d792ba32b0d",
                "9305a450451056a0c87ef9b3b645d5a8ce6670daa06d14de5438bbdad76914de",
                "87142c83298e86a334afd39e1166d28b8987b4c46d98993e36f6ad0a9d522ab3",
                "b30a36cad744fba8e1080d0e03686ad9cd3921ef5a5e0d1d5239efcef5693207",
            ],
        ),
        (
            "demo-landseq.toml",
            &[
                "05b8ec79e4e5313b8d3a2b6cd21d3dea660f52c151ebb21542d4097c8d097e2c",
                "9c0c2ee266ec1ba0b3fb1f94edc3d03211c30c78ed7a3342dac2292b6286f1a8",
                "e857fe08dae5e7ca16d63f74ae275b1186f45b02f8f1488429df7ac33d842ae9",
            ],
        ),
        (
            "replay/agents.toml",
            &[
                "ea5c928506ed1c3a89b2d56c3b6cbc389a03494eab920fb02ef4400e14c6fad5",
                "6dc067aabc645ff233e9011a13b62fe6619b380dcdc9aff6649c496443d409b4",
                "3ebaf11cf7591f6eadd0e665b582f99b6050369362992225fe342af69f75b6a4",
                "619b2f874f96191bc3591656ad6d8b6d052fb68064cdbb3c6230d11b34b426d6",
            ],
        ),
        (
            "replay/hono.toml",
            &[
                "6ee507a5cc41192acd4d441be78ac75860806f470fa19ef96f45c0b1b05a7f29",
                "c026fcccec6222873f99fc98eaaea791b1b866e57cd305702d425241e34a5dcf",
                "8c854bd1b02767760bdb440e5a5e247d96efd49f933da5f3ba48a8bd6080abfa",
                "9239c0cdf4630c351e60ba47e66fb99a595baababe86618998258544b089af1f",
            ],
        ),
        (
            "replay/landseq.toml",
            &[
                "d6c3c14bc9ac7c3d0ad5043802a42cb767bd8616b96e3178265e7ab2c891cb78",
                "5ec0312a65d46a3d4d3b4ae96faee53fc250760e77b06c8c028556773faa5222",
                "a3afbd6a50832f6c54f1ba4e4a00ea2cae7de5a63dd10c4abc5dee2d288b8ed2",
                "2b61aee5f432832a2c656d30fc28364cd62b45488660be2b2c6d41ffd372cf70",
            ],
        ),
        (
            "rich",
            &[
                "4cdccc412bd59c8508c9eddb90f5a22384658216f6da537ea79f396ac68ac78e",
                "16147f531edb5a8491aa28aca1c37d6d8339f2842e5a44f7bb848b39ca2a1398",
                "6d81288f68ef19631cc11335e8c8b88454c1e3c07959e20c6fd7408663162fa0",
            ],
        ),
        (
            "gated",
            &[
                "515d8efe26c99d42c4b366288dbd1ad83d4555dfcab23c0eb105e068436edc8d",
                "c091d1daacee207688131c9ab9624e9010f17c1a37d167c4125b79b47a4c91b2",
            ],
        ),
    ];
    let texts = texts();
    for (name, want) in pinned {
        let text = &texts.iter().find(|(n, _)| *n == name).unwrap().1;
        let rules = Rules::parse(text).unwrap();
        assert_eq!(rules.digests(), want, "{name}");
        assert_eq!(rules.digest(), want[0], "{name}: the canonical digest comes first");
    }
}

#[test]
fn canonical_json_has_sorted_keys_and_no_defaults() {
    let hono = Rules::parse(&texts()[3].1).unwrap();
    let json = String::from_utf8(hono.canonical()).unwrap();
    assert!(json.starts_with(r#"{"approvers":[{"key":""#), "{json}");
    assert!(!json.contains("[]") && !json.contains("null") && !json.contains("\"ci\"") && !json.contains(' '), "{json}");
    assert_eq!(Rules::default().canonical(), b"{}", "every field of Rules is left out at its default");
}

/// `Rules` as a later landseq may declare it: four more fields, each with a default.
#[derive(Serialize, Deserialize)]
struct Grown {
    #[serde(flatten)]
    rules: Rules,
    #[serde(default)]
    mirrors: Vec<String>,
    #[serde(default)]
    strict: bool,
    #[serde(default)]
    note: Option<String>,
    #[serde(default = "seven")]
    window: u32,
}

fn seven() -> u32 {
    7
}

#[test]
fn a_new_field_with_a_default_keeps_the_digest_of_files_that_do_not_use_it() {
    for (name, text) in texts() {
        let now = Rules::parse(&text).unwrap();
        let later: Grown = toml::from_str(&text).unwrap();
        assert_eq!(canonical_json(&later), now.canonical(), "{name}");
        let explicit: Grown = toml::from_str(&format!("window = 7\nstrict = false\n{text}")).unwrap();
        assert_eq!(canonical_json(&explicit), now.canonical(), "{name}: a default written out is still the default");
        let uses: Grown = toml::from_str(&format!("window = 9\n{text}")).unwrap();
        assert_ne!(canonical_json(&uses), now.canonical(), "{name}: a file that sets the new field has a new digest");
    }
}

#[test]
fn a_field_is_left_out_only_at_its_own_default() {
    let base = format!("{}\n[decide]\nprovider = \"clef\"\n", include_str!("../../fixtures/legacy/bench-landseq.toml"));
    let d = |extra: &str| Rules::parse(&format!("{base}{extra}")).unwrap().digest();
    assert_eq!(d(""), d("[decide.risk]\nthreshold = 0.8\n"), "0.8 is the default threshold");
    assert_ne!(d(""), d("[decide.risk]\nthreshold = 0.9\n"));
    assert_ne!(d(""), d("model = \"\"\n"), "an empty model is not an absent one");
}

#[test]
fn an_earlier_form_counts_only_when_it_holds_every_rule() {
    let texts = texts();
    let bench = &texts[0].1;
    let plain = Rules::parse(bench).unwrap();
    let gated = Rules::parse(&bench.replace("[seals]\n", "[seals]\napproval = [\"**\"]\n")).unwrap();
    let typed = Rules::parse(&format!("{bench}\n[ci]\ntypecheck = {{ tsconfig = \"tsconfig.json\" }}\n")).unwrap();
    assert_eq!(gated.digests().len(), 2, "only 962ae9a's form has seals.approval");
    assert_eq!(typed.digests().len(), 3, "e2523c3's form has no [ci]");
    for d in plain.digests() {
        assert!(plain.accepts(&d));
        assert!(!gated.accepts(&d) && !typed.accepts(&d), "an approval of the plain rules approves no edit of them");
    }
    for d in gated.digests().iter().chain(&typed.digests()) {
        assert!(!plain.accepts(d));
    }
}
