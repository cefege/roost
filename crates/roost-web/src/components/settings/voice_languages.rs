//! The Deepgram `nova-3` language list, verbatim from the official support
//! matrix. Ports `LANGUAGES` in
//! `apps/web/src/components/Settings/TranscriptionPane.tsx`; the Settings voice
//! pane is the only reader.
//!
//! A CLOSED list, not free text: an unsupported code makes Deepgram reject the
//! socket, so a typo here is a broken microphone rather than a rejected setting.
//! `multi` does live code-switching across en/es/fr/de/hi/ru/pt/ja/it/nl, and
//! `__auto__` detects. English is first because it is the default and the only
//! mode keyterm biasing applies to.

use crate::components::md::SelectOption;

/// `(code, label)`, in the coordinator's own order.
pub const LANGUAGES: [(&str, &str); 89] = [
    ("en", "English"),
    ("en-US", "English (US)"),
    ("en-GB", "English (UK)"),
    ("en-AU", "English (Australia)"),
    ("en-IN", "English (India)"),
    ("en-NZ", "English (New Zealand)"),
    ("ar", "Arabic"),
    ("ar-EG", "Arabic (Egypt)"),
    ("ar-SA", "Arabic (Saudi Arabia)"),
    ("ar-AE", "Arabic (UAE)"),
    ("ar-QA", "Arabic (Qatar)"),
    ("ar-KW", "Arabic (Kuwait)"),
    ("ar-SY", "Arabic (Syria)"),
    ("ar-LB", "Arabic (Lebanon)"),
    ("ar-PS", "Arabic (Palestine)"),
    ("ar-JO", "Arabic (Jordan)"),
    ("ar-SD", "Arabic (Sudan)"),
    ("ar-TD", "Arabic (Chad)"),
    ("ar-MA", "Arabic (Morocco)"),
    ("ar-DZ", "Arabic (Algeria)"),
    ("ar-TN", "Arabic (Tunisia)"),
    ("ar-IQ", "Arabic (Iraq)"),
    ("ar-IR", "Arabic (Iran)"),
    ("be", "Belarusian"),
    ("bn", "Bengali"),
    ("bs", "Bosnian"),
    ("bg", "Bulgarian"),
    ("ca", "Catalan"),
    ("zh", "Chinese (Mandarin, Simplified)"),
    ("zh-CN", "Chinese (Mandarin, Simplified) [zh-CN]"),
    ("zh-Hans", "Chinese (Mandarin, Simplified) [zh-Hans]"),
    ("zh-TW", "Chinese (Mandarin, Traditional)"),
    ("zh-Hant", "Chinese (Mandarin, Traditional) [zh-Hant]"),
    ("zh-HK", "Chinese (Cantonese, Traditional)"),
    ("hr", "Croatian"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("da-DK", "Danish (Denmark)"),
    ("nl", "Dutch"),
    ("nl-BE", "Flemish"),
    ("et", "Estonian"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("fr-CA", "French (Canada)"),
    ("de", "German"),
    ("de-CH", "German (Switzerland)"),
    ("el", "Greek"),
    ("gu", "Gujarati"),
    ("gu-IN", "Gujarati (India)"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hu", "Hungarian"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("kn", "Kannada"),
    ("ko", "Korean"),
    ("ko-KR", "Korean (Korea)"),
    ("lv", "Latvian"),
    ("lt", "Lithuanian"),
    ("mk", "Macedonian"),
    ("ms", "Malay"),
    ("mr", "Marathi"),
    ("no", "Norwegian"),
    ("fa", "Persian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("pt-PT", "Portuguese (Portugal)"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sr", "Serbian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("es", "Spanish"),
    ("es-419", "Spanish (Latin America)"),
    ("sv", "Swedish"),
    ("sv-SE", "Swedish (Sweden)"),
    ("tl", "Tagalog"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("th", "Thai"),
    ("th-TH", "Thai (Thailand)"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("vi", "Vietnamese"),
    ("multi", "Multilingual (code-switching)"),
    ("__auto__", "Auto-detect"),
];

/// The language options the picker offers.
pub fn language_options() -> Vec<SelectOption> {
    LANGUAGES
        .iter()
        .map(|(code, label)| SelectOption::new(*code, *label))
        .collect()
}

/// The stored language, defaulting to English when the coordinator stored none.
pub fn stored_or_default(language: &str) -> String {
    if language.trim().is_empty() {
        "en".to_owned()
    } else {
        language.trim().to_owned()
    }
}
