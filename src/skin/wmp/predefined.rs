//! Predefined elements and colour names.
//!
//! `<PLAYBUTTON>` and its kin are ordinary elements with attributes already
//! filled in, each overridable, exactly as the skin programming reference
//! lists them, so they read here as a table and not as special cases.

use super::super::Rgb;

/// The element a tag is made from, and the attributes it comes with.
pub fn expand(tag: &str) -> (&'static str, &'static [(&'static str, &'static str)]) {
    match tag {
        "playbutton" | "playelement" => (
            base(tag),
            &[
                ("onclick", "player.controls.play();"),
                ("uptooltip", "Play"),
                ("enabled", "wmpenabled:player.controls.play"),
            ],
        ),
        "pausebutton" | "pauseelement" => (
            base(tag),
            &[
                ("onclick", "player.controls.pause();"),
                ("uptooltip", "Pause"),
                ("enabled", "wmpenabled:player.controls.pause"),
            ],
        ),
        "stopbutton" | "stopelement" => (
            base(tag),
            &[
                ("onclick", "player.controls.stop();"),
                ("uptooltip", "Stop"),
                ("enabled", "wmpenabled:player.controls.stop"),
            ],
        ),
        "prevbutton" | "prevelement" => (
            base(tag),
            &[
                ("onclick", "player.controls.previous();"),
                ("uptooltip", "Previous"),
                ("enabled", "wmpenabled:player.controls.previous"),
            ],
        ),
        "nextbutton" | "nextelement" => (
            base(tag),
            &[
                ("onclick", "player.controls.next();"),
                ("uptooltip", "Next"),
                ("enabled", "wmpenabled:player.controls.next"),
            ],
        ),
        "ffwdbutton" | "ffwdelement" => (
            base(tag),
            &[
                ("onclick", "player.controls.fastForward();"),
                ("uptooltip", "Fast Forward"),
                ("enabled", "wmpenabled:player.controls.fastForward"),
            ],
        ),
        "rewbutton" | "rewelement" => (
            base(tag),
            &[
                ("onclick", "player.controls.fastReverse();"),
                ("uptooltip", "Fast Reverse"),
                ("enabled", "wmpenabled:player.controls.fastReverse"),
            ],
        ),
        "mutebutton" => (
            "button",
            &[
                ("onclick", "player.settings.mute=down;"),
                ("uptooltip", "Mute"),
                ("downtooltip", "Sound"),
                ("down", "wmpprop:player.settings.mute"),
                ("sticky", "true"),
            ],
        ),
        "repeatbutton" => (
            "button",
            &[
                ("onclick", "player.settings.setMode('loop',down);"),
                ("uptooltip", "Turn Repeat On"),
                ("downtooltip", "Turn Repeat Off"),
                ("down", "wmpprop:player.settings.getMode('loop')"),
                ("sticky", "true"),
            ],
        ),
        "shufflebutton" => (
            "button",
            &[
                ("onclick", "player.settings.setMode('shuffle',down);"),
                ("uptooltip", "Turn Shuffle On"),
                ("downtooltip", "Turn Shuffle Off"),
                ("down", "wmpprop:player.settings.getMode('shuffle')"),
                ("sticky", "true"),
            ],
        ),
        "closebutton" => (
            "button",
            &[("onclick", "view.close();"), ("uptooltip", "Close")],
        ),
        "minimizebutton" => (
            "button",
            &[("onclick", "view.minimize();"), ("uptooltip", "Minimize")],
        ),
        "returnbutton" => (
            "button",
            &[
                ("onclick", "view.returnToMediaCenter();"),
                ("uptooltip", "Return to Full Mode"),
            ],
        ),
        "imagebutton" => ("button", &[("cursor", "hand")]),
        "seekslider" => (
            "slider",
            &[
                ("tooltip", "Seek"),
                ("min", "0"),
                ("max", "wmpprop:player.currentMedia.duration"),
                ("value", "wmpprop:player.controls.currentPosition"),
                (
                    "ondragend",
                    "jscript:player.controls.currentPosition=value;",
                ),
            ],
        ),
        "volumeslider" => (
            "slider",
            &[
                ("tooltip", "Volume"),
                ("min", "0"),
                ("max", "100"),
                ("value", "wmpprop:player.settings.volume"),
                (
                    "value_onchange",
                    "jscript:player.settings.volume=value; player.settings.mute=false;",
                ),
            ],
        ),
        "balanceslider" => (
            "slider",
            &[
                ("tooltip", "Balance"),
                ("min", "-100"),
                ("max", "100"),
                ("value", "wmpprop:player.settings.balance"),
                ("value_onchange", "jscript:player.settings.balance=value;"),
            ],
        ),
        "progressbar" => ("slider", &[]),
        "currentpositiontext" => (
            "text",
            &[
                ("value", "wmpprop:player.controls.currentPositionString"),
                ("justification", "right"),
            ],
        ),
        "durationtext" => (
            "text",
            &[
                ("value", "wmpprop:player.currentMedia.durationString"),
                ("justification", "right"),
            ],
        ),
        "statustext" => ("text", &[("value", "wmpprop:player.status")]),
        "tracknametext" => ("text", &[("value", "wmpprop:player.currentMedia.name")]),
        "dropdownplaylist" => ("playlist", &[("playlistitemsvisible", "false")]),
        "itemsplaylist" => (
            "playlist",
            &[
                ("backgroundcolor", "black"),
                ("foregroundcolor", "white"),
                ("columns", "name=Name;Duration=Time"),
                ("columnsvisible", "false"),
                ("dropdownvisible", "false"),
            ],
        ),
        "wmpeffects" => (
            "effects",
            &[
                ("horizontalalignment", "stretch"),
                ("verticalalignment", "stretch"),
                ("width", "250"),
                ("height", "200"),
                ("onclick", "next();"),
            ],
        ),
        "wmpvideo" => (
            "video",
            &[
                ("backgroundcolor", "black"),
                ("horizontalalignment", "stretch"),
                ("verticalalignment", "stretch"),
            ],
        ),
        "popup" => ("listbox", &[("popup", "true")]),
        "volume" | "balance" => ("other", &[]),
        other => (intern(other), &[]),
    }
}

/// A button tag is a `button`, an element one a `buttonelement`.
fn base(tag: &str) -> &'static str {
    if tag.ends_with("element") {
        "buttonelement"
    } else {
        "button"
    }
}

fn intern(tag: &str) -> &'static str {
    match tag {
        "view" => "view",
        "subview" => "subview",
        "button" => "button",
        "buttongroup" => "buttongroup",
        "buttonelement" => "buttonelement",
        "slider" => "slider",
        "customslider" => "customslider",
        "text" => "text",
        "video" => "video",
        "effects" => "effects",
        "playlist" => "playlist",
        "listbox" => "listbox",
        "editbox" => "editbox",
        "player" => "player",
        "network" | "controls" | "settings" => "player",
        "equalizersettings" => "equalizersettings",
        "videosettings" => "videosettings",
        "item" => "item",
        "column" => "column",
        "automenu" => "automenu",
        _ => "other",
    }
}

/// `#rrggbb`, `#rgb`, `0xrrggbb`, `rrggbb` or one of the named colours.
pub fn color(value: &str) -> Option<Rgb> {
    let value = value.trim();
    let digits = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    let channel = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    let parsed = match digits.len() {
        6 => (|| Some([channel(0)?, channel(2)?, channel(4)?]))(),
        3 => (|| {
            let nibble = |at: usize| u8::from_str_radix(digits.get(at..at + 1)?, 16).ok();
            Some([nibble(0)? * 17, nibble(1)? * 17, nibble(2)? * 17])
        })(),
        _ => None,
    };
    parsed.or_else(|| {
        let name = value.to_ascii_lowercase();
        NAMED
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, code)| [(code >> 16) as u8, (code >> 8) as u8, *code as u8])
    })
}

const NAMED: &[(&str, u32)] = &[
    // System colours, at Windows' classic defaults.
    ("activeborder", 0xD4D0C8),
    ("activecaption", 0x0A246A),
    ("appworkspace", 0x808080),
    ("background", 0x3A6EA5),
    ("buttonface", 0xD4D0C8),
    ("buttonhighlight", 0xFFFFFF),
    ("buttonshadow", 0x808080),
    ("buttontext", 0x000000),
    ("captiontext", 0xFFFFFF),
    ("graytext", 0x808080),
    ("highlight", 0x0A246A),
    ("highlighttext", 0xFFFFFF),
    ("inactiveborder", 0xD4D0C8),
    ("inactivecaption", 0x808080),
    ("inactivecaptiontext", 0xD4D0C8),
    ("infobackground", 0xFFFFE1),
    ("infotext", 0x000000),
    ("menu", 0xD4D0C8),
    ("menutext", 0x000000),
    ("scrollbar", 0xD4D0C8),
    ("threeddarkshadow", 0x404040),
    ("threedface", 0xD4D0C8),
    ("threedhighlight", 0xFFFFFF),
    ("threedlightshadow", 0xD4D0C8),
    ("threedshadow", 0x808080),
    ("window", 0xFFFFFF),
    ("windowframe", 0x000000),
    ("windowtext", 0x000000),
    ("aliceblue", 0xF0F8FF),
    ("antiquewhite", 0xFAEBD7),
    ("aqua", 0x00FFFF),
    ("aquamarine", 0x7FFFD4),
    ("azure", 0xF0FFFF),
    ("beige", 0xF5F5DC),
    ("bisque", 0xFFE4C4),
    ("black", 0x000000),
    ("blanchedalmond", 0xFFEBCD),
    ("blue", 0x0000FF),
    ("blueviolet", 0x8A2BE2),
    ("brown", 0xA52A2A),
    ("burlywood", 0xDEB887),
    ("cadetblue", 0x5F9EA0),
    ("chartreuse", 0x7FFF00),
    ("chocolate", 0xD2691E),
    ("coral", 0xFF7F50),
    ("cornflowerblue", 0x6495ED),
    ("cornsilk", 0xFFF8DC),
    ("crimson", 0xDC143C),
    ("cyan", 0x00FFFF),
    ("darkblue", 0x00008B),
    ("darkcyan", 0x008B8B),
    ("darkgoldenrod", 0xB8860B),
    ("darkgray", 0xA9A9A9),
    ("darkgreen", 0x006400),
    ("darkkhaki", 0xBDB76B),
    ("darkmagenta", 0x8B008B),
    ("darkolivegreen", 0x556B2F),
    ("darkorange", 0xFF8C00),
    ("darkorchid", 0x9932CC),
    ("darkred", 0x8B0000),
    ("darksalmon", 0xE9967A),
    ("darkseagreen", 0x8FBC8F),
    ("darkslateblue", 0x483D8B),
    ("darkslategray", 0x2F4F4F),
    ("darkturquoise", 0x00CED1),
    ("darkviolet", 0x9400D3),
    ("deeppink", 0xFF1493),
    ("deepskyblue", 0x00BFFF),
    ("dimgray", 0x696969),
    ("dodgerblue", 0x1E90FF),
    ("firebrick", 0xB22222),
    ("floralwhite", 0xFFFAF0),
    ("forestgreen", 0x228B22),
    ("fuchsia", 0xFF00FF),
    ("gainsboro", 0xDCDCDC),
    ("ghostwhite", 0xF8F8FF),
    ("gold", 0xFFD700),
    ("goldenrod", 0xDAA520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xADFF2F),
    ("honeydew", 0xF0FFF0),
    ("hotpink", 0xFF69B4),
    ("indianred", 0xCD5C5C),
    ("indigo", 0x4B0082),
    ("ivory", 0xFFFFF0),
    ("khaki", 0xF0E68C),
    ("lavender", 0xE6E6FA),
    ("lavenderblush", 0xFFF0F5),
    ("lawngreen", 0x7CFC00),
    ("lemonchiffon", 0xFFFACD),
    ("lightblue", 0xADD8E6),
    ("lightcoral", 0xF08080),
    ("lightcyan", 0xE0FFFF),
    ("lightgoldenrodyellow", 0xFAFAD2),
    ("lightgreen", 0x90EE90),
    ("lightgrey", 0xD3D3D3),
    ("lightgray", 0xD3D3D3),
    ("lightpink", 0xFFB6C1),
    ("lightsalmon", 0xFFA07A),
    ("lightseagreen", 0x20B2AA),
    ("lightskyblue", 0x87CEFA),
    ("lightslategray", 0x778899),
    ("lightsteelblue", 0xB0C4DE),
    ("lightyellow", 0xFFFFE0),
    ("lime", 0x00FF00),
    ("limegreen", 0x32CD32),
    ("linen", 0xFAF0E6),
    ("magenta", 0xFF00FF),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66CDAA),
    ("mediumblue", 0x0000CD),
    ("mediumorchid", 0xBA55D3),
    ("mediumpurple", 0x9370DB),
    ("mediumseagreen", 0x3CB371),
    ("mediumslateblue", 0x7B68EE),
    ("mediumspringgreen", 0x00FA9A),
    ("mediumturquoise", 0x48D1CC),
    ("mediumvioletred", 0xC71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xF5FFFA),
    ("mistyrose", 0xFFE4E1),
    ("moccasin", 0xFFE4B5),
    ("navajowhite", 0xFFDEAD),
    ("navy", 0x000080),
    ("oldlace", 0xFDF5E6),
    ("olive", 0x808000),
    ("olivedrab", 0x6B8E23),
    ("orange", 0xFFA500),
    ("orangered", 0xFF4500),
    ("orchid", 0xDA70D6),
    ("palegoldenrod", 0xEEE8AA),
    ("palegreen", 0x98FB98),
    ("paleturquoise", 0xAFEEEE),
    ("palevioletred", 0xDB7093),
    ("papayawhip", 0xFFEFD5),
    ("peachpuff", 0xFFDAB9),
    ("peru", 0xCD853F),
    ("pink", 0xFFC0CB),
    ("plum", 0xDDA0DD),
    ("powderblue", 0xB0E0E6),
    ("purple", 0x800080),
    ("red", 0xFF0000),
    ("rosybrown", 0xBC8F8F),
    ("royalblue", 0x4169E1),
    ("saddlebrown", 0x8B4513),
    ("salmon", 0xFA8072),
    ("sandybrown", 0xF4A460),
    ("seagreen", 0x2E8B57),
    ("seashell", 0xFFF5EE),
    ("sienna", 0xA0522D),
    ("silver", 0xC0C0C0),
    ("skyblue", 0x87CEEB),
    ("slateblue", 0x6A5ACD),
    ("slategray", 0x708090),
    ("snow", 0xFFFAFA),
    ("springgreen", 0x00FF7F),
    ("steelblue", 0x4682B4),
    ("tan", 0xD2B48C),
    ("teal", 0x008080),
    ("thistle", 0xD8BFD8),
    ("tomato", 0xFF6347),
    ("turquoise", 0x40E0D0),
    ("violet", 0xEE82EE),
    ("wheat", 0xF5DEB3),
    ("white", 0xFFFFFF),
    ("whitesmoke", 0xF5F5F5),
    ("yellow", 0xFFFF00),
    ("yellowgreen", 0x9ACD32),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_come_in_the_forms_skins_write() {
        assert_eq!(color("#FF00ff"), Some([255, 0, 255]));
        assert_eq!(color("0x0a0b0c"), Some([10, 11, 12]));
        assert_eq!(color("#f80"), Some([255, 136, 0]));
        assert_eq!(color("Red"), Some([255, 0, 0]));
        assert_eq!(color("beige"), Some([245, 245, 220]));
        assert_eq!(color("none"), None);
    }

    #[test]
    fn a_predefined_element_is_a_plain_one_with_attributes() {
        assert_eq!(expand("playelement").0, "buttonelement");
        assert_eq!(expand("playbutton").0, "button");
        assert_eq!(expand("mutebutton").0, "button");
        assert_eq!(expand("seekslider").0, "slider");
        assert_eq!(expand("wmpeffects").0, "effects");
        assert_eq!(expand("somethingelse").0, "other");
        assert!(expand("stopbutton").1.contains(&("uptooltip", "Stop")));
    }
}
