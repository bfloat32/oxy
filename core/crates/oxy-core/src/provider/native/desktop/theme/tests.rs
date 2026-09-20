use super::*;
use serde_json::json;

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oxy-theme-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn roots_at(base: &Path) -> Roots {
    Roots {
        user: base.join("user/themes"),
        system: base.join("system/themes"),
    }
}

/// `slug` already lowercases and dashes the name, so the fixture takes
/// the slug verbatim.
fn write_theme(roots: &Roots, user: bool, slug: &str, colors: &str, shell: Option<&str>) {
    let dir = (if user { &roots.user } else { &roots.system }).join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("colors.toml"), colors).unwrap();
    if let Some(s) = shell {
        std::fs::write(dir.join("shell.toml"), s).unwrap();
    }
}

fn rows_of(out: NativeOutcome) -> Vec<Value> {
    match out {
        NativeOutcome::Rows(rows) => rows,
        _ => panic!("wanted rows"),
    }
}

fn decode(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    for quad in bytes.chunks(4) {
        let val = |b: u8| -> u32 {
            match b {
                b'A'..=b'Z' => u32::from(b - b'A'),
                b'a'..=b'z' => u32::from(b - b'a' + 26),
                b'0'..=b'9' => u32::from(b - b'0' + 52),
                b'+' => 62,
                b'/' => 63,
                _ => 0,
            }
        };
        let n = val(quad[0]) << 18
            | val(quad[1]) << 12
            | val(*quad.get(2).unwrap_or(&b'=')) << 6
            | val(*quad.get(3).unwrap_or(&b'='));
        out.push((n >> 16) as u8);
        if quad.len() > 2 && quad[2] != b'=' {
            out.push((n >> 8) as u8);
        }
        if quad.len() > 3 && quad[3] != b'=' {
            out.push(n as u8);
        }
    }
    out
}

#[test]
fn base64_known_vectors() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foob"), "Zm9vYg==");
    assert_eq!(base64(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
}

#[test]
fn base64_round_trips_arbitrary_bytes() {
    let data: Vec<u8> = (0u8..=255).collect();
    assert_eq!(decode(&base64(&data)), data);
    // A colors.toml is text; the round trip has to be byte-exact anyway
    // because applyTheme decodes it back into a file.
    assert_eq!(
        decode(&base64(b"accent = \"#88c0d0\"\n")),
        b"accent = \"#88c0d0\"\n"
    );
}

#[test]
fn squash_drops_only_space_and_dash() {
    assert_eq!(squash("tokyo-night"), "tokyonight");
    assert_eq!(squash("tokyo night"), "tokyonight");
    assert_eq!(squash("ever_forest"), "ever_forest");
    assert_eq!(squash(""), "");
}

#[test]
fn slug_is_the_theme_set_translation() {
    assert_eq!(slug("Tokyo Night"), "tokyo-night");
    assert_eq!(slug("GRUVBOX"), "gruvbox");
    assert_eq!(slug("A  B"), "a--b");
}

#[test]
fn theme_dir_prefers_the_users_own() {
    let base = tmp("dirs");
    let roots = roots_at(&base);
    // No theme dirs at all: the shipped path answers.
    assert_eq!(roots.theme_dir("Nord"), base.join("system/themes/nord"));
    // A dir under ~/.config/omarchy/themes wins.
    std::fs::create_dir_all(roots.user.join("nord")).unwrap();
    assert_eq!(roots.theme_dir("Nord"), base.join("user/themes/nord"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parse_flat_reads_the_pairs_the_regex_reads() {
    let c = parse_flat("accent = \"#88c0d0\"\nmode=\"light\"\nempty = \"\"\ndim = \"1\" extra\n");
    assert_eq!(c["accent"], "#88c0d0");
    assert_eq!(c["mode"], "light");
    assert_eq!(c["empty"], "");
    // The value ends at the next quote; the tail of the line is ignored.
    assert_eq!(c["dim"], "1");
}

#[test]
fn parse_flat_skips_what_the_regex_skips() {
    let c = parse_flat(
        " accent = \"#fff\"\n[section]\nkey = unquoted\nkey2 = \"y\"\nUPPER = \"z\"\n\
         # comment\n= \"v\"\n",
    );
    assert!(c.is_empty());
    // A digit is not a key character, so "key2" never reaches the `=`.
    let c = parse_flat("key2 = \"y\"\n");
    assert!(!c.contains_key("key") && !c.contains_key("key2"));
    // A tab is not a space — ` *=` does not match it.
    let c = parse_flat("key\t= \"y\"\n");
    assert!(!c.contains_key("key"));
}

#[test]
fn parse_flat_last_duplicate_wins() {
    let c = parse_flat("a = \"1\"\na = \"2\"\n");
    assert_eq!(c["a"], "2");
}

#[test]
fn theme_mode_reads_the_first_lowercase_value() {
    let base = tmp("mode");
    let dir = base.join("t");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("colors.toml"),
        "xmode = \"dark\"\nmode = \"light\"\n",
    )
    .unwrap();
    assert_eq!(theme_mode(&dir), "light");
    std::fs::write(dir.join("colors.toml"), "mode=\"dark\"\n").unwrap();
    assert_eq!(theme_mode(&dir), "dark");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn theme_mode_defaults_dark_where_the_regex_fails() {
    let base = tmp("mode2");
    let dir = base.join("t");
    std::fs::create_dir_all(&dir).unwrap();
    // No file at all.
    assert_eq!(theme_mode(&dir), "dark");
    // Uppercase is not `[a-z]+`; neither is a number; a leading space
    // keeps `^mode` from matching.
    for text in [
        "mode = \"Light\"\n",
        "mode = \"123\"\n",
        "  mode = \"light\"\n",
        "mode = \"\"\n",
    ] {
        std::fs::write(dir.join("colors.toml"), text).unwrap();
        assert_eq!(theme_mode(&dir), "dark", "{text:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn cap_value_is_the_bash_arithmetic() {
    assert_eq!(cap_value(None), Some(DEFAULT_LIMIT));
    assert_eq!(cap_value(Some("")), Some(DEFAULT_LIMIT));
    assert_eq!(cap_value(Some("12")), Some(12));
    assert_eq!(cap_value(Some(" -3 ")), Some(-3));
    // A bare word is a variable reference: unset reads as 0, and a 0
    // cap emits no rows.
    assert_eq!(cap_value(Some("lots")), Some(0));
    // Anything else fails the `[[ -gt ]]` outright — uncapped, not zero.
    assert_eq!(cap_value(Some("12x")), None);
    assert_eq!(cap_value(Some("  ")), None);
}

#[test]
fn raw_limit_prefers_settings_then_env() {
    let mut m = Map::new();
    m.insert("theme_limit".into(), json!(7));
    assert_eq!(raw_limit(Some(&m)), Some("7".to_string()));
    let mut m = Map::new();
    m.insert("theme_limit".into(), json!("9"));
    assert_eq!(raw_limit(Some(&m)), Some("9".to_string()));
    // null is skipped the way `settings_prefix` skips it, so the
    // environment is what remains — whatever this box has.
    let mut m = Map::new();
    m.insert("theme_limit".into(), Value::Null);
    assert_eq!(raw_limit(Some(&m)), std::env::var("OXY_THEME_LIMIT").ok());
    assert_eq!(
        raw_limit(Some(&Map::new())),
        std::env::var("OXY_THEME_LIMIT").ok()
    );
}

#[test]
fn retint_is_the_applytheme_call() {
    let base = tmp("retint");
    let dir = base.join("t");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("colors.toml"), "accent = \"#aabbcc\"\n").unwrap();
    std::fs::write(dir.join("shell.toml"), "prompt = \"x\"\n").unwrap();
    let expected = format!(
        "omarchy-shell shell applyTheme {} {}; oxy-theme-preview {}",
        quote(&base64(b"accent = \"#aabbcc\"\n")),
        quote(&base64(b"prompt = \"x\"\n")),
        quote(&dir.to_string_lossy()),
    );
    assert_eq!(retint(&dir), expected);
    // The base64 must decode back to the files themselves.
    let cmd = retint(&dir);
    let parts: Vec<&str> = cmd.split('\'').collect();
    assert_eq!(decode(parts[1]), b"accent = \"#aabbcc\"\n");
    assert_eq!(decode(parts[3]), b"prompt = \"x\"\n");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn retint_declines_without_colors() {
    let base = tmp("nocolors");
    let dir = base.join("t");
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(retint(&dir), "");
    // shell.toml is optional; colors.toml is not.
    std::fs::write(dir.join("shell.toml"), "x").unwrap();
    assert_eq!(retint(&dir), "");
    let _ = std::fs::remove_dir_all(&base);
}

const COLORS: &str = "mode = \"light\"\nbackground = \"#eff1f5\"\nforeground = \"#4c4f69\"\
    \ndark_foreground = \"#6c6f85\"\naccent = \"#8839ef\"\nlighter_background = \"#e6e9ef\"\
    \nred = \"#d20f39\"\nyellow = \"#df8e1d\"\ngreen = \"#40a02b\"\ncyan = \"#179299\"\
    \nblue = \"#1e66f5\"\nmagenta = \"#ea76cb\"\n";

#[test]
fn the_answer_orders_current_first_and_caps() {
    let base = tmp("answer");
    let roots = roots_at(&base);
    write_theme(&roots, true, "current-one", COLORS, Some("s"));
    write_theme(&roots, false, "alpha", COLORS, None);
    write_theme(&roots, false, "beta", COLORS, None);
    let out = answer_at(
        &roots,
        "Alpha\nBeta\nCurrent One\n",
        "Current One",
        "",
        Some(2),
    );
    let rows = rows_of(out);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], json!("Current One"));
    assert_eq!(rows[0]["current"], json!(true));
    assert_eq!(rows[0]["subtitle"], json!("Current theme"));
    assert_eq!(rows[0]["score"], json!(95000));
    assert_eq!(rows[0]["total"], json!(3));
    assert_eq!(rows[0]["shown"], json!(2));
    assert_eq!(rows[1]["id"], json!("Alpha"));
    assert_eq!(rows[1]["current"], json!(false));
    assert_eq!(rows[1]["subtitle"], json!("Theme"));
    assert_eq!(rows[1]["score"], json!(89900));
    // Enter — and only Enter — is `omarchy theme set`, quoted.
    assert_eq!(rows[1]["exec"], json!("omarchy theme set 'Alpha'"));
    // Revert is the arrived-on theme's retint, on every row.
    let back = format!(
        "omarchy-shell shell applyTheme {} {}; oxy-theme-preview {}",
        quote(&base64(COLORS.as_bytes())),
        quote(&base64(b"s")),
        quote(&roots.theme_dir("Current One").to_string_lossy()),
    );
    assert_eq!(rows[1]["revertExec"], json!(back));
    assert!(
        rows[1]["previewExec"]
            .as_str()
            .unwrap()
            .starts_with("omarchy-shell shell applyTheme ")
    );
    assert!(
        rows[1]["previewExec"]
            .as_str()
            .unwrap()
            .contains("; oxy-theme-preview ")
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn light_matches_names_and_modes_but_nothing_else() {
    let base = tmp("light");
    let roots = roots_at(&base);
    write_theme(&roots, false, "flexoki-light", "mode = \"dark\"\n", None);
    write_theme(&roots, false, "nord", COLORS, None);
    write_theme(&roots, false, "gruvbox", "mode = \"dark\"\n", None);
    // "dark" matches every dark-mode theme — Flexoki Light's name does not
    // say so, its colors.toml does.
    let rows = rows_of(answer_at(
        &roots,
        "Flexoki Light\nGruvbox\nNord\n",
        "",
        "dark",
        Some(48),
    ));
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids, ["Flexoki Light", "Gruvbox"]);
    // "light" keeps the name and the mode both.
    let rows = rows_of(answer_at(
        &roots,
        "Flexoki Light\nGruvbox\nNord\n",
        "",
        "light",
        Some(48),
    ));
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids, ["Flexoki Light", "Nord"]);
    assert_eq!(rows[1]["mode"], json!("light"));
    // A query that is not light/dark never opens colors.toml: "nord"
    // matches the name and nothing more.
    let rows = rows_of(answer_at(
        &roots,
        "Flexoki Light\nGruvbox\nNord\n",
        "",
        "rd",
        Some(48),
    ));
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids, ["Nord"]);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn squashed_spellings_match() {
    let base = tmp("squash");
    let roots = roots_at(&base);
    write_theme(&roots, false, "tokyo-night", COLORS, None);
    let rows = rows_of(answer_at(
        &roots,
        "Tokyo Night\n",
        "",
        "tokyonight",
        Some(48),
    ));
    assert_eq!(rows[0]["id"], json!("Tokyo Night"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn no_match_is_empty_and_a_missing_colors_file_is_unpainted() {
    let base = tmp("empty");
    let roots = roots_at(&base);
    write_theme(&roots, false, "nord", COLORS, None);
    assert!(matches!(
        answer_at(&roots, "Nord\n", "", "zzz", Some(48)),
        NativeOutcome::Empty
    ));
    // A listed theme with no directory at all still answers — unpainted
    // and unpreviewable, which is the honest card.
    let rows = rows_of(answer_at(&roots, "Ghost\nNord\n", "", "", Some(48)));
    assert_eq!(rows[0]["mode"], json!("dark"));
    assert_eq!(rows[0]["bg"], json!(""));
    assert_eq!(rows[0]["swatches"], json!([]));
    assert_eq!(rows[0]["previewExec"], json!(""));
    // No `current` on the box: no row claims it, and revert has no
    // colors file to work with.
    assert_eq!(rows[0]["current"], json!(false));
    assert_eq!(rows[0]["revertExec"], json!(""));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn zero_cap_is_no_rows() {
    let base = tmp("zerocap");
    let roots = roots_at(&base);
    write_theme(&roots, false, "nord", COLORS, None);
    assert!(matches!(
        answer_at(&roots, "Nord\n", "", "", Some(0)),
        NativeOutcome::Empty
    ));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn row_fields_are_the_view_contract() {
    let base = tmp("fields");
    let roots = roots_at(&base);
    write_theme(&roots, false, "nord", COLORS, None);
    let rows = rows_of(answer_at(&roots, "Nord\n", "", "", Some(48)));
    let r = &rows[0];
    assert_eq!(r["mode"], json!("light"));
    assert_eq!(r["bg"], json!("#eff1f5"));
    assert_eq!(r["fg"], json!("#4c4f69"));
    assert_eq!(r["dim"], json!("#6c6f85"));
    assert_eq!(r["accent"], json!("#8839ef"));
    assert_eq!(r["surface"], json!("#e6e9ef"));
    assert_eq!(
        r["swatches"],
        json!([
            "#d20f39", "#df8e1d", "#40a02b", "#179299", "#1e66f5", "#ea76cb"
        ])
    );
    // The fallbacks: muted under dim, selection under surface.
    let base2 = tmp("fields2");
    let roots2 = roots_at(&base2);
    write_theme(
        &roots2,
        false,
        "nord",
        "muted = \"#aaa\"\nselection = \"#bbb\"\n",
        None,
    );
    let rows = rows_of(answer_at(&roots2, "Nord\n", "", "", Some(48)));
    assert_eq!(rows[0]["dim"], json!("#aaa"));
    assert_eq!(rows[0]["surface"], json!("#bbb"));
    assert_eq!(rows[0]["swatches"], json!([]));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&base2);
}
