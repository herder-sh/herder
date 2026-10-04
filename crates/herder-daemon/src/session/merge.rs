//! The text of queued prompts merged into one.
//!
//! Each prompt's `[Image #N]` markers count its own images from 1; in the merged prompt, which
//! carries all their images in order, they count across it.

const MARKER: &str = "[Image #";

/// Joins `prompts`, each a text and how many images it carries, with a blank line between
/// them, renumbering each one's `[Image #N]` markers past the images of those before it. A
/// marker naming none of its prompt's images stays as it is.
pub(super) fn merge_texts<'a>(prompts: impl IntoIterator<Item = (&'a str, usize)>) -> String {
    let mut texts = Vec::new();
    let mut before = 0;
    for (text, images) in prompts {
        texts.push(renumber(text, images, before));
        before += images;
    }
    texts.join("\n\n")
}

/// `text` with each marker of one of its `images` moved up by `by`.
fn renumber(text: &str, images: usize, by: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(MARKER) {
        let after = &rest[start + MARKER.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let number = after[..digits]
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=images).contains(n) && after[digits..].starts_with(']'));
        match number {
            Some(n) => {
                out.push_str(&rest[..start]);
                out.push_str(&format!("{MARKER}{}]", n + by));
                rest = &after[digits + 1..];
            }
            None => {
                out.push_str(&rest[..start + MARKER.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::merge_texts;

    #[test]
    fn markers_count_across_the_merged_prompt() {
        let merged = merge_texts([
            ("look at [Image #1] and [Image #2]", 2),
            ("no images here", 0),
            ("[Image #1] then [Image #3], [Image #2]", 3),
        ]);
        assert_eq!(
            merged,
            "look at [Image #1] and [Image #2]\n\nno images here\n\n\
             [Image #3] then [Image #5], [Image #4]"
        );
    }

    #[test]
    fn markers_naming_no_image_of_their_prompt_stay() {
        let merged = merge_texts([
            ("[Image #1]", 1),
            ("[Image #2] [Image #0] [Image #x] [Image #1 [Image #1]", 1),
        ]);
        assert_eq!(
            merged,
            "[Image #1]\n\n[Image #2] [Image #0] [Image #x] [Image #1 [Image #2]"
        );
    }
}
