//! Parser conformance for each supported interchange format and malformed input.
//!
//! Fixtures cover every supported format and representative malformed input.
//! Runtime parsing stays in the parent module.

use super::*;

fn windows_json() -> String {
    let mut value = serde_json::json!({
        "name": "test",
        "foreground": format!("#{:06x}", 0xf0f0f0),
        "background": format!("#{:06x}", 0x101010)
    });
    let names = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "purple",
        "cyan",
        "white",
        "brightBlack",
        "brightRed",
        "brightGreen",
        "brightYellow",
        "brightBlue",
        "brightPurple",
        "brightCyan",
        "brightWhite",
    ];
    for (index, name) in names.iter().enumerate() {
        value.as_object_mut().unwrap().insert(
            (*name).to_owned(),
            serde_json::Value::String(format!("#{index:06x}")),
        );
    }
    value.to_string()
}

fn iterm_xml() -> String {
    fn append_color(xml: &mut String, name: &str) {
        xml.push_str(&format!("<key>{name}</key><dict>"));
        xml.push_str("<key>Red Component</key><real>0</real>");
        xml.push_str("<key>Green Component</key><real>0.5</real>");
        xml.push_str("<key>Blue Component</key><real>1</real></dict>");
    }
    let mut xml = String::from("<?xml version=\"1.0\"?><plist><dict>");
    for index in 0..16 {
        append_color(&mut xml, &format!("Ansi {index} Color"));
    }
    append_color(&mut xml, "Foreground Color");
    append_color(&mut xml, "Background Color");
    xml.push_str("</dict></plist>");
    xml
}

#[test]
fn reads_iterm2_xml_plist_component_triplets() {
    let scheme = parse_scheme(&iterm_xml()).expect("well-formed iTerm plist");
    assert_eq!(scheme.palette.ansi[0], Rgb(0, 128, 255));
    assert_eq!(scheme.palette.background, Rgb(0, 128, 255));
}

#[test]
fn rejects_malformed_iterm_xml() {
    assert_eq!(
        parse_scheme("<?xml version=\"1.0\"?><plist><dict><key>broken"),
        Err(SchemeParseError::InvalidFormat)
    );
}

#[test]
fn reads_windows_terminal_json_and_defaults_optional_special_colors() {
    let scheme = parse_scheme(&windows_json()).expect("well-formed fixture");
    assert_eq!(scheme.name, "test");
    assert_eq!(scheme.palette.ansi[15], Rgb(0, 0, 15));
    assert_eq!(scheme.palette.cursor, scheme.palette.foreground);
}

#[test]
fn reads_kitty_and_ghostty_key_value_spellings() {
    let mut conf = format!(
        "foreground #{:06x}\nbackground #{:06x}\ncursor #{:06x}\n",
        0xf0f0f0, 0x101010, 0xabcdef
    );
    for index in 0..16 {
        conf.push_str(&format!("color{index} {index:06x}\n"));
    }
    let scheme = parse_scheme(&conf).expect("valid key/value palette");
    assert_eq!(scheme.palette.cursor, Rgb(0xab, 0xcd, 0xef));
}

#[test]
fn rejects_invalid_input_and_incomplete_key_value_palettes() {
    assert_eq!(
        parse_scheme("{bad json}"),
        Err(SchemeParseError::InvalidJson)
    );
    assert!(matches!(
        parse_scheme("foreground red"),
        Err(SchemeParseError::MissingField("ANSI colors 0–15"))
    ));
}
#[test]
fn reads_ghostty_equals_and_palette_lines() {
    let mut conf = format!(
        "foreground = #{:06x}\nbackground = #{:06x}\ncursor-color = #{:06x}\n",
        0xf0f0f0, 0x101010, 0xabcdef
    );
    for index in 0..16 {
        conf.push_str(&format!("palette = {index}=#{index:06x}\n"));
    }
    let scheme = parse_scheme(&conf).expect("valid Ghostty palette");
    assert_eq!(scheme.palette.ansi[15], Rgb(0, 0, 15));
    assert_eq!(scheme.palette.cursor, Rgb(0xab, 0xcd, 0xef));
}

#[test]
fn exposes_all_nine_named_presets() {
    let names: Vec<_> = built_in_schemes()
        .iter()
        .map(|scheme| scheme.name.clone())
        .collect();
    assert_eq!(
        names,
        [
            "Solarized Light",
            "Solarized Dark",
            "Dracula",
            "Gruvbox Dark",
            "Nord",
            "Catppuccin Latte",
            "Catppuccin Mocha",
            "Tokyo Night",
            "One Dark"
        ]
    );
}
