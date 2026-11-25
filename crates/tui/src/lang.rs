//! The language of the TUI's own words (labels, hints, status).
//!
//! Chinese unless the locale says English: `LYNSHEN_LANG=zh|en` wins, then
//! the first set of `LC_ALL`, `LC_MESSAGES`, `LANG`. Command names, model
//! names and the engine's own messages are not translated.

#[cfg(not(test))]
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Zh,
    En,
}

#[cfg(not(test))]
static LANG: OnceLock<Lang> = OnceLock::new();

pub(crate) fn lang() -> Lang {
    #[cfg(test)]
    {
        TEST_LANG.with(|cell| cell.get())
    }
    #[cfg(not(test))]
    {
        *LANG.get_or_init(|| detect(|name| std::env::var(name).ok()))
    }
}

/// Picks the Chinese or the English wording.
pub(crate) fn t(zh: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

pub(crate) fn is_zh() -> bool {
    lang() == Lang::Zh
}

pub(crate) fn detect(var: impl Fn(&str) -> Option<String>) -> Lang {
    let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    if let Some(forced) = set("LYNSHEN_LANG") {
        let forced = forced.trim().to_ascii_lowercase();
        if forced.starts_with("en") {
            return Lang::En;
        }
        if forced.starts_with("zh") {
            return Lang::Zh;
        }
    }
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| set(name))
        .unwrap_or_default();
    if locale.to_ascii_lowercase().starts_with("en") {
        Lang::En
    } else {
        Lang::Zh
    }
}

#[cfg(test)]
thread_local! {
    static TEST_LANG: std::cell::Cell<Lang> = const { std::cell::Cell::new(Lang::Zh) };
}

/// Runs `f` with `lang` on this thread (tests default to Chinese).
#[cfg(test)]
pub(crate) fn with_lang<T>(lang: Lang, f: impl FnOnce() -> T) -> T {
    let before = TEST_LANG.with(|cell| cell.replace(lang));
    let result = f();
    TEST_LANG.with(|cell| cell.set(before));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn chinese_unless_the_locale_is_english() {
        assert_eq!(detect(env(&[])), Lang::Zh);
        assert_eq!(detect(env(&[("LANG", "ja_JP.UTF-8")])), Lang::Zh);
        assert_eq!(detect(env(&[("LANG", "zh_CN.UTF-8")])), Lang::Zh);
        assert_eq!(detect(env(&[("LANG", "en_US.UTF-8")])), Lang::En);
        assert_eq!(
            detect(env(&[("LC_ALL", "C.UTF-8"), ("LANG", "en_US.UTF-8")])),
            Lang::Zh
        );
    }

    #[test]
    fn lynshen_lang_overrides_the_locale() {
        assert_eq!(
            detect(env(&[("LYNSHEN_LANG", "en"), ("LANG", "zh_CN.UTF-8")])),
            Lang::En
        );
        assert_eq!(
            detect(env(&[("LYNSHEN_LANG", "zh"), ("LANG", "en_US.UTF-8")])),
            Lang::Zh
        );
    }
}
