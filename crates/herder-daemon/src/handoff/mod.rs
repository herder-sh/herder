//! Replay handoff: the transcript a new provider session is seeded with, sized to fit.
//!
//! A session moves to a fresh CLI process (a daemon restart, or later an account or provider
//! switch) by replaying its journal's items as the [`StartRequest::seed`]. A long session's
//! journal outgrows the target model's context, so [`transcript`] condenses it to a token
//! budget from [`budget`]:
//!
//! 1. A transcript that fits is returned unchanged.
//! 2. Otherwise reasoning is dropped (no adapter replays it), and tool outputs longer than
//!    [`TRUNCATE_ABOVE`] characters are cut to their head and tail around a
//!    `[… N chars elided …]` marker, oldest turn first, stopping as soon as it fits. The newest
//!    [`PROTECTED_TURNS`] turns are left whole.
//! 3. If it still does not fit, whole turns are dropped, oldest first, always keeping the
//!    opening request (the first user message) and the newest [`PROTECTED_TURNS`] turns.
//! 4. Only when those turns alone are too big are their tool outputs cut, oldest first, then
//!    their oldest turns dropped, then the newest turn's own oldest items. A tool call and its
//!    result are always dropped together.
//! 5. Whenever anything was dropped, a [`NOTE`] user message goes right after the opening
//!    request, so the agent knows history is missing.
//!
//! Tokens are estimated as bytes / 4 plus a small per-item overhead: providers' tokenizers are
//! not available locally, and bytes over-count non-ASCII text, which errs on the safe side.
//!
//! [`StartRequest::seed`]: herder_adapters::StartRequest::seed

use herder_protocol::{Item, ItemBody, ItemId, Provider, TurnId};

#[cfg(test)]
mod tests;

/// Tool outputs longer than this many characters are cut down when a transcript is condensed.
pub const TRUNCATE_ABOVE: usize = 4_000;

/// Newest turns whose tool outputs are cut only when they alone exceed the budget.
pub const PROTECTED_TURNS: usize = 3;

/// Characters kept from each end of a cut tool output.
const KEEP_EACH_END: usize = 1_500;

/// Tokens counted per item on top of its text, for role and framing.
const ITEM_OVERHEAD: usize = 8;

/// Tells the agent that the transcript it was handed is not the whole history.
pub const NOTE: &str = "[herder: earlier history of this conversation was condensed to fit the \
                        context window; some older turns are left out.]";

/// Id of the [`NOTE`] item; never journaled, so it cannot clash with an adapter's ids.
const NOTE_ID: &str = "herder-handoff-note";

/// Seed budgets in tokens by provider and model family: the first row whose provider matches
/// and whose pattern occurs in the model name wins; an empty pattern matches every model.
/// Conservative, well under each family's context window, so the new session has room to work.
const BUDGETS: &[(&str, &str, usize)] = &[
    ("claude", "[1m]", 400_000),
    ("claude", "", 100_000),
    ("codex", "", 100_000),
    ("gemini", "", 200_000),
    ("grok", "", 100_000),
    ("cursor", "", 50_000),
    ("opencode", "", 50_000),
];

/// Budget for a provider or model family not in [`BUDGETS`].
const DEFAULT_BUDGET: usize = 50_000;

/// Seed budget in tokens for a session on `provider` running `model` (empty for the default).
pub fn budget(provider: &Provider, model: &str) -> usize {
    BUDGETS
        .iter()
        .find(|(name, pattern, _)| *name == provider.as_str() && model.contains(pattern))
        .map_or(DEFAULT_BUDGET, |(_, _, tokens)| *tokens)
}

/// Estimated tokens `items` take when replayed.
pub fn estimate(items: &[Item]) -> usize {
    items.iter().map(cost).sum()
}

/// The journal's `items`, oldest first, condensed to at most `budget` tokens as the module
/// describes. The result may exceed `budget` only when the opening request alone does.
pub fn transcript(items: Vec<Item>, budget: usize) -> Vec<Item> {
    if estimate(&items) <= budget {
        return items;
    }
    let mut items: Vec<Item> = items
        .into_iter()
        .filter(|item| !matches!(item.body, ItemBody::Reasoning { .. } | ItemBody::Unknown))
        .collect();
    let head = items
        .iter()
        .position(|item| matches!(item.body, ItemBody::UserMessage { .. }))
        .map(|index| items.remove(index));
    let mut turns = turns(items);
    let mut total =
        head.as_ref().map_or(0, cost) + turns.iter().map(|t| estimate(t)).sum::<usize>();
    let older = turns.len().saturating_sub(PROTECTED_TURNS);
    shorten(&mut turns[..older], &mut total, budget);
    if total <= budget {
        return head
            .into_iter()
            .chain(turns.into_iter().flatten())
            .collect();
    }

    let note = Item {
        id: ItemId::new(NOTE_ID),
        turn_id: head
            .as_ref()
            .map_or_else(|| TurnId::new(NOTE_ID), |head| head.turn_id.clone()),
        body: ItemBody::UserMessage { text: NOTE.into() },
    };
    total += cost(&note);
    while total > budget && turns.len() > PROTECTED_TURNS {
        total -= estimate(&turns.remove(0));
    }
    shorten(&mut turns, &mut total, budget);
    while total > budget && turns.len() > 1 {
        total -= estimate(&turns.remove(0));
    }
    if let Some(newest) = turns.first_mut() {
        while total > budget && !newest.is_empty() {
            total -= drop_oldest(newest);
        }
    }
    head.into_iter()
        .chain(std::iter::once(note))
        .chain(turns.into_iter().flatten())
        .collect()
}

/// Cuts long tool outputs in `turns`, oldest first, until `total` fits `budget`, keeping
/// `total` current.
fn shorten(turns: &mut [Vec<Item>], total: &mut usize, budget: usize) {
    for item in turns.iter_mut().flatten() {
        if *total <= budget {
            return;
        }
        let before = cost(item);
        truncate(item);
        *total -= before.saturating_sub(cost(item));
    }
}

/// Estimated tokens of one item.
fn cost(item: &Item) -> usize {
    let bytes = match &item.body {
        ItemBody::UserMessage { text }
        | ItemBody::AssistantMessage { text }
        | ItemBody::Reasoning { text } => text.len(),
        ItemBody::ToolCall { name, input } => name.len() + input.to_string().len(),
        ItemBody::ToolResult { output, .. } => output.len(),
        ItemBody::Unknown => 0,
    };
    bytes / 4 + ITEM_OVERHEAD
}

/// Cuts a long tool output down to its head and tail around an elision marker.
fn truncate(item: &mut Item) {
    if let ItemBody::ToolResult { output, .. } = &mut item.body {
        let chars = output.chars().count();
        if chars > TRUNCATE_ABOVE {
            let head: String = output.chars().take(KEEP_EACH_END).collect();
            let tail: String = output.chars().skip(chars - KEEP_EACH_END).collect();
            let elided = chars - 2 * KEEP_EACH_END;
            *output = format!("{head}\n[… {elided} chars elided …]\n{tail}");
        }
    }
}

/// Items split into runs of the same turn, in order; the journal keeps a turn's items together.
fn turns(items: Vec<Item>) -> Vec<Vec<Item>> {
    let mut turns: Vec<Vec<Item>> = Vec::new();
    for item in items {
        match turns.last_mut() {
            Some(turn) if turn[0].turn_id == item.turn_id => turn.push(item),
            _ => turns.push(vec![item]),
        }
    }
    turns
}

/// Drops a turn's oldest item, with the result answering it when it is a tool call; returns
/// the tokens freed.
fn drop_oldest(turn: &mut Vec<Item>) -> usize {
    let first = turn.remove(0);
    let mut freed = cost(&first);
    if matches!(first.body, ItemBody::ToolCall { .. }) {
        turn.retain(|item| {
            let answer = matches!(
                &item.body,
                ItemBody::ToolResult { call_id, .. } if *call_id == first.id
            );
            if answer {
                freed += cost(item);
            }
            !answer
        });
    }
    freed
}
