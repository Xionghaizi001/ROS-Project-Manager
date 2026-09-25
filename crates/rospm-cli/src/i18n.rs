//! Locale selection and message rendering (spec §4.2, decision #16).
//!
//! The language is decided before clap parses the command line so `--help`
//! is localised too. Resources live in `locales/{en,zh-CN}.yml` and are
//! compiled into the binary by `rust_i18n::i18n!` (invoked in `main.rs`).
//!
//! `rospm-core` produces [`Message`]s (keys + arguments); [`render`] turns
//! them into text and is installed as the core's renderer by [`install`].

use std::borrow::Cow;

use rospm_core::message::Message;

/// Languages with a resource file. `en` is the source language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    ZhCn,
}

impl Lang {
    /// The rust-i18n locale code (also the resource file stem).
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::ZhCn => "zh-CN",
        }
    }

    /// Map a locale-ish value (`zh_CN.UTF-8`, `zh`, `en_US`, `C`, …) to a
    /// language: anything starting with `zh` is Chinese, anything starting
    /// with `en` is English, everything else is "no opinion".
    fn from_value(value: &str) -> Option<Lang> {
        let v = value.trim();
        if v.starts_with("zh") {
            Some(Lang::ZhCn)
        } else if v.starts_with("en") {
            Some(Lang::En)
        } else {
            None
        }
    }
}

/// Pick the language from the command line and the environment:
/// `--lang` > `ROSPM_LANG` > `LANGUAGE` (colon-separated, first match) >
/// `LC_ALL` > `LC_MESSAGES` > `LANG`. `zh*` → Chinese; anything else
/// (including unset, `C`, `POSIX`) → English.
pub fn detect<I, S, E>(args: I, env: E) -> Lang
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    E: Fn(&str) -> Option<String>,
{
    if let Some(v) = lang_flag(args) {
        if let Some(l) = Lang::from_value(&v) {
            return l;
        }
    }
    let set = |k: &str| env(k).filter(|v| !v.trim().is_empty());
    if let Some(v) = set("ROSPM_LANG") {
        return Lang::from_value(&v).unwrap_or(Lang::En);
    }
    if let Some(list) = set("LANGUAGE") {
        if let Some(l) = list.split(':').find_map(Lang::from_value) {
            return l;
        }
    }
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(v) = set(key) {
            return if v.starts_with("zh") {
                Lang::ZhCn
            } else {
                Lang::En
            };
        }
    }
    Lang::En
}

/// The value of `--lang VALUE` / `--lang=VALUE` in raw arguments (no clap
/// yet). Stops at `--`.
fn lang_flag<I, S>(args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let a = a.as_ref();
        if a == "--" {
            return None;
        }
        if a == "--lang" {
            return it.next().map(|v| v.as_ref().to_string());
        }
        if let Some(v) = a.strip_prefix("--lang=") {
            return Some(v.to_string());
        }
    }
    None
}

/// Decide from the real process arguments and environment.
pub fn from_process() -> Lang {
    detect(std::env::args().skip(1), |k| std::env::var(k).ok())
}

/// Activate `lang` for this process and hand the renderer to rospm-core.
pub fn install(lang: Lang) {
    rust_i18n::set_locale(lang.code());
    rospm_core::message::set_renderer(render);
}

/// The active language.
pub fn current() -> Lang {
    if rust_i18n::locale().starts_with("zh") {
        Lang::ZhCn
    } else {
        Lang::En
    }
}

/// Raw translated text for `key` in the active language, falling back to
/// English. `None` when the key exists in neither (a bug; callers show the
/// key so nothing is lost).
fn lookup(key: &str) -> Option<Cow<'static, str>> {
    let backend = crate::_rust_i18n_backend();
    let locale = rust_i18n::locale();
    if let Some(text) = backend.translate(&locale, key) {
        return Some(text);
    }
    let fallback = backend.translate("en", key)?;
    if &*locale != "en" {
        tracing::info!("i18n: `{key}` has no {} text, using en", &*locale);
    }
    Some(fallback)
}

/// Translate a key without arguments (CLI strings, clap descriptions).
pub fn tr(key: &str) -> String {
    match lookup(key) {
        Some(text) => text.into_owned(),
        None => {
            tracing::warn!("i18n: missing key `{key}`");
            key.to_string()
        }
    }
}

/// Render a core message in the active language. Verbatim text passes
/// through; keys are looked up and their `%{name}` placeholders filled.
pub fn render(m: &Message) -> String {
    match m {
        Message::Text(s) => s.clone(),
        Message::Key { key, args } => match lookup(key) {
            Some(text) => {
                let names: Vec<&str> = args.iter().map(|(k, _)| *k).collect();
                let values: Vec<String> = args.iter().map(|(_, v)| v.clone()).collect();
                rust_i18n::replace_patterns(&text, &names, &values)
            }
            None => {
                tracing::warn!("i18n: missing key `{key}`");
                m.developer_form()
            }
        },
    }
}

/// Terminal columns a string occupies, counting East Asian wide characters
/// as two so tables line up in both languages.
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) >= 0x2E80 { 2 } else { 1 })
        .sum()
}

/// Left-align `s` in `width` terminal columns.
pub fn pad(s: &str, width: usize) -> String {
    let w = display_width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    fn no_args() -> Vec<&'static str> {
        Vec::new()
    }

    #[test]
    fn locale_variables_pick_the_language() {
        assert_eq!(
            detect(no_args(), env_of(&[("LANG", "zh_CN.UTF-8")])),
            Lang::ZhCn
        );
        assert_eq!(detect(no_args(), env_of(&[("LANG", "zh_TW")])), Lang::ZhCn);
        assert_eq!(detect(no_args(), env_of(&[("LANG", "C")])), Lang::En);
        assert_eq!(detect(no_args(), env_of(&[("LANG", "POSIX")])), Lang::En);
        assert_eq!(
            detect(no_args(), env_of(&[("LANG", "en_US.UTF-8")])),
            Lang::En
        );
        assert_eq!(detect(no_args(), env_of(&[])), Lang::En);
        assert_eq!(detect(no_args(), env_of(&[("LANG", "")])), Lang::En);
    }

    #[test]
    fn language_list_takes_the_first_match() {
        assert_eq!(
            detect(no_args(), env_of(&[("LANGUAGE", "zh:en"), ("LANG", "C")])),
            Lang::ZhCn
        );
        assert_eq!(
            detect(
                no_args(),
                env_of(&[("LANGUAGE", "en:zh"), ("LANG", "zh_CN.UTF-8")])
            ),
            Lang::En
        );
        // Unknown entries are skipped, then the LC_* chain applies.
        assert_eq!(
            detect(no_args(), env_of(&[("LANGUAGE", "fr:zh_CN")])),
            Lang::ZhCn
        );
        assert_eq!(
            detect(no_args(), env_of(&[("LANGUAGE", "fr"), ("LANG", "zh_CN")])),
            Lang::ZhCn
        );
    }

    #[test]
    fn precedence_is_lc_all_then_lc_messages_then_lang() {
        assert_eq!(
            detect(
                no_args(),
                env_of(&[("LC_ALL", "C"), ("LC_MESSAGES", "zh_CN"), ("LANG", "zh_CN")])
            ),
            Lang::En
        );
        assert_eq!(
            detect(
                no_args(),
                env_of(&[("LC_MESSAGES", "zh_CN.UTF-8"), ("LANG", "en_US")])
            ),
            Lang::ZhCn
        );
    }

    #[test]
    fn overrides_win_in_order() {
        let env = env_of(&[("ROSPM_LANG", "en"), ("LANG", "zh_CN.UTF-8")]);
        assert_eq!(detect(no_args(), &env), Lang::En);
        assert_eq!(detect(["check", "--lang", "zh"], &env), Lang::ZhCn);
        assert_eq!(detect(["--lang=zh", "check"], &env), Lang::ZhCn);
        assert_eq!(
            detect(
                no_args(),
                env_of(&[("ROSPM_LANG", "zh"), ("LANGUAGE", "en")])
            ),
            Lang::ZhCn
        );
        // An unusable --lang value is left for clap to reject; it does not
        // silently pick a language.
        assert_eq!(
            detect(["--lang", "xx"], env_of(&[("LANG", "zh_CN")])),
            Lang::ZhCn
        );
        assert_eq!(
            detect(["--", "--lang", "zh"], env_of(&[("LANG", "C")])),
            Lang::En
        );
    }

    /// Flatten a locale file into `a.b.c` keys.
    fn keys_of(yaml: &str) -> std::collections::BTreeMap<String, String> {
        fn walk(
            prefix: &str,
            v: &serde_yaml::Value,
            out: &mut std::collections::BTreeMap<String, String>,
        ) {
            match v {
                serde_yaml::Value::Mapping(m) => {
                    for (k, v) in m {
                        let k = k.as_str().expect("string key");
                        let key = if prefix.is_empty() {
                            k.to_string()
                        } else {
                            format!("{prefix}.{k}")
                        };
                        walk(&key, v, out);
                    }
                }
                serde_yaml::Value::String(s) => {
                    out.insert(prefix.to_string(), s.clone());
                }
                other => panic!("{prefix}: unexpected value {other:?}"),
            }
        }
        let v: serde_yaml::Value = serde_yaml::from_str(yaml).expect("valid YAML");
        let mut out = std::collections::BTreeMap::new();
        walk("", &v, &mut out);
        out
    }

    fn placeholders(text: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let mut rest = text;
        while let Some(start) = rest.find("%{") {
            let after = &rest[start + 2..];
            let end = after.find('}').expect("closed placeholder");
            out.insert(after[..end].to_string());
            rest = &after[end + 1..];
        }
        out
    }

    #[test]
    fn locales_have_identical_keys_and_placeholders() {
        let en = keys_of(include_str!("../locales/en.yml"));
        let zh = keys_of(include_str!("../locales/zh-CN.yml"));
        // `_test.*` keys exist only in en to exercise the fallback path.
        let en_keys: Vec<&String> = en.keys().filter(|k| !k.starts_with("_test.")).collect();
        let zh_keys: Vec<&String> = zh.keys().collect();
        let missing_in_zh: Vec<&&String> =
            en_keys.iter().filter(|k| !zh.contains_key(**k)).collect();
        let missing_in_en: Vec<&&String> =
            zh_keys.iter().filter(|k| !en.contains_key(**k)).collect();
        assert!(
            missing_in_zh.is_empty() && missing_in_en.is_empty(),
            "missing in zh-CN: {missing_in_zh:?}\nmissing in en: {missing_in_en:?}"
        );
        for k in en_keys {
            assert_eq!(
                placeholders(&en[k]),
                placeholders(&zh[k]),
                "placeholders differ for `{k}`"
            );
        }
        // The compiled-in backend agrees with the files.
        let mut compiled: Vec<String> = crate::_rust_i18n_available_locales()
            .into_iter()
            .map(|l| l.into_owned())
            .collect();
        compiled.sort();
        assert_eq!(compiled, vec!["en".to_string(), "zh-CN".to_string()]);
    }

    #[test]
    fn missing_keys_fall_back_to_english_without_panicking() {
        rust_i18n::set_locale("zh-CN");
        assert_eq!(current(), Lang::ZhCn);
        // Present only in en.yml: rendered from the English text.
        assert_eq!(tr("_test.en_only"), "english only");
        assert_eq!(render(&rospm_core::msg!("_test.en_only")), "english only");
        // Present nowhere: the key (developer form) comes back, no panic.
        assert_eq!(tr("_test.absent"), "_test.absent");
        assert_eq!(
            render(&rospm_core::msg!("_test.absent", n = 1)),
            "_test.absent(n=1)"
        );
        // Placeholders are filled in the selected language.
        assert_eq!(
            render(&rospm_core::msg!("plan.write_file", path = "/x", bytes = 3)),
            "写入 /x（3 字节）"
        );
        assert_eq!(render(&Message::text("as is")), "as is");
        rust_i18n::set_locale("en");
        assert_eq!(
            render(&rospm_core::msg!("plan.write_file", path = "/x", bytes = 3)),
            "write /x (3 bytes)"
        );
    }

    #[test]
    fn width_counts_cjk_as_two_columns() {
        assert_eq!(display_width("ok"), 2);
        assert_eq!(display_width("缺失"), 4);
        assert_eq!(pad("缺失", 8), "缺失    ");
        assert_eq!(pad("MISSING", 8), "MISSING ");
    }
}
