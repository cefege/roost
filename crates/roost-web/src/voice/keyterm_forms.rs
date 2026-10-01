//! The shape of a token: what is worth a keyterm, and how a recognizer is told
//! to hear it.
//!
//! Split out of `super::keyterms` because these are word-level rules with no
//! scoring and no context: whether a term is a term, and whether a variant can
//! be spoken, are decided the same way whatever screen the word came from.

/// How much structure a token has, which is what makes it a term rather than a
/// word: `coordFactory` and `TAILSCALE` are names, `build` is not.
#[must_use]
pub fn structural_bonus(token: &str) -> f64 {
    let mut bonus = 1.0;
    if has_camel_case(token) {
        bonus += 1.5;
    }
    if token.contains('_') {
        bonus += 1.2;
    }
    if has_kebab_case(token) {
        bonus += 0.8;
    }
    if token.chars().any(|character| character.is_ascii_digit()) {
        bonus += 0.5;
    }
    if token.len() >= 2 && token.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return bonus + 1.0;
    }
    if is_capitalised_word(token) {
        bonus += 0.7;
    }
    bonus
}

fn has_camel_case(token: &str) -> bool {
    token.char_indices().skip(1).any(|(index, character)| {
        character.is_ascii_uppercase()
            && token[..index]
                .chars()
                .next_back()
                .is_some_and(|previous| previous.is_ascii_lowercase())
    })
}

fn has_kebab_case(token: &str) -> bool {
    token.split_once('-').is_some_and(|(head, tail)| {
        !head.is_empty()
            && !tail.is_empty()
            && head.chars().all(|character| character.is_ascii_lowercase())
            && tail
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    })
}

pub fn is_capitalised_word(token: &str) -> bool {
    let mut characters = token.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_uppercase())
        && characters.all(|character| character.is_ascii_lowercase())
}

/// Whether a token is worth scoring at all.
///
/// Rejected: pure numbers, anything without a letter, the common English and
/// shell vocabulary, and a hex-looking identifier, which is almost always an
/// address or a build hash and is never spoken.
#[must_use]
pub fn keep(token: &str) -> bool {
    if token.len() < 2 || token.len() > 40 {
        return false;
    }
    if !token
        .chars()
        .any(|character| character.is_ascii_alphabetic())
    {
        return false;
    }
    if token.chars().all(|character| character.is_ascii_digit()) {
        return false;
    }
    if looks_like_hex_id(token) {
        return false;
    }
    !super::keyterm_stopwords::is_stopword(token)
}

fn looks_like_hex_id(token: &str) -> bool {
    token.len() >= 6
        && token.chars().any(|character| character.is_ascii_digit())
        && token.chars().all(|character| character.is_ascii_hexdigit())
}

/// The phrase a recognizer should hear for a token, or `None` when the token is
/// already sayable as it stands.
///
/// `coordFactory` is heard as two words and `compose_dictation` as two more; a
/// token the engine already pronounces is not worth a variant's bytes.
#[must_use]
pub fn spoken_form(token: &str) -> Option<String> {
    if !is_speakable(token) {
        return None;
    }
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut index = 0usize;
    for character in token.chars() {
        if matches!(character, '_' | '-' | '/' | '.') {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        // An uppercase letter starts a new word only when a lowercase letter
        // follows it: that is the camelCase boundary. `TAILSCALE` is one word
        // and `coordFactory` is two.
        if character.is_ascii_uppercase() && !current.is_empty() && next_is_lower(token, index) {
            words.push(std::mem::take(&mut current));
        }
        current.push(character);
        index += character.len_utf8();
    }
    if !current.is_empty() {
        words.push(current);
    }
    let spoken = words
        .iter()
        .map(|word| word.to_lowercase())
        .collect::<Vec<String>>()
        .join(" ");
    if spoken == token.to_lowercase() || !spoken.contains(' ') {
        return None;
    }
    Some(spoken)
}

/// Whether the character after byte `index` is a lowercase letter.
pub fn next_is_lower(token: &str, index: usize) -> bool {
    token[index..]
        .chars()
        .nth(1)
        .is_some_and(|next| next.is_ascii_lowercase())
}

pub fn is_speakable(token: &str) -> bool {
    if token.is_empty()
        || !token
            .chars()
            .all(|character| ('\x20'..='\x7e').contains(&character))
    {
        return false;
    }
    let letters = token.chars().filter(char::is_ascii_alphabetic).count();
    if letters < 2 || letters * 2 < token.len() {
        return false;
    }
    token
        .chars()
        .filter(|character| !character.is_ascii_alphanumeric() && *character != ' ')
        .count()
        <= 2
}

/// How many words a keyterm is worth charging the URL for.
#[must_use]
pub fn token_count(term: &str) -> usize {
    let camel_words = split_camel(term)
        .iter()
        .filter(|word| !word.is_empty())
        .count();
    let separated = term
        .split([' ', '.', '_', '/', '@', '-'])
        .filter(|piece| !piece.is_empty())
        .count();
    camel_words.max(separated).max(1)
}

pub fn split_camel(term: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in term.chars() {
        if character.is_ascii_uppercase() && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    words.push(current);
    words
}

/// The tokens of a text, split on the characters terminal output uses between
/// words.
pub fn tokens(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        let continues =
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '/' | '@' | '-');
        if continues {
            current.push(character);
        } else if !current.is_empty() {
            found.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        found.push(current);
    }
    found
}

/// Strip the punctuation and path decoration a token carries on screen but is
/// not said with.
#[must_use]
pub fn normalize(raw: &str) -> String {
    let trimmed = raw.trim_matches(|character: char| {
        matches!(
            character,
            '^' | '('
                | '"'
                | '\''
                | '['
                | '{'
                | '<'
                | ')'
                | ']'
                | '}'
                | '>'
                | '.'
                | ','
                | ';'
                | ':'
                | '!'
                | '?'
        )
    });
    let basename = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    strip_extension(basename).to_owned()
}

fn strip_extension(name: &str) -> &str {
    const EXTENSIONS: [&str; 19] = [
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "md", "css", "scss", "sh", "rs", "py",
        "go", "txt", "log", "toml", "yaml", "html",
    ];
    match name.rsplit_once('.') {
        Some((stem, extension))
            if EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()) =>
        {
            stem
        }
        _ => name,
    }
}
