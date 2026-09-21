//! Output helpers shared by the command modules and the menu.
use crate::exit::{self, Failure, fail};

/// Prints a value as the JSON document of a `--json` command.
pub(crate) fn json(value: &impl serde::Serialize) -> Result<(), Failure> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|_| fail(exit::INTERNAL, "OUTPUT_ENCODING_FAILED"))?;
    println!("{text}");
    Ok(())
}

/// Names come from users and from subscriptions. Control characters could
/// rewrite earlier terminal output, so each becomes a space before display.
pub(crate) fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_cannot_reach_the_terminal() {
        assert_eq!(plain("a\u{1b}[2Jb\r\nc\t"), "a [2Jb  c ");
        assert_eq!(plain("普通 名称"), "普通 名称");
    }
}
