use std::collections::HashSet;

use serde_json::json;

use super::*;

fn item(id: String, turn: usize, body: ItemBody) -> Item {
    Item {
        id: ItemId::new(id),
        turn_id: TurnId::new(format!("turn-{turn}")),
        body,
    }
}

/// One turn: a prompt, reasoning, a tool call with an `output_chars` long result, a reply.
fn turn(n: usize, output_chars: usize) -> Vec<Item> {
    vec![
        item(
            format!("{n}-user"),
            n,
            ItemBody::UserMessage {
                text: format!("Request {n}"),
                attachments: Vec::new(),
            },
        ),
        item(
            format!("{n}-think"),
            n,
            ItemBody::Reasoning {
                text: "thinking ".repeat(50),
            },
        ),
        item(
            format!("{n}-call"),
            n,
            ItemBody::ToolCall {
                name: "Bash".into(),
                input: json!({"command": format!("cargo test {n}")}),
            },
        ),
        item(
            format!("{n}-result"),
            n,
            ItemBody::ToolResult {
                call_id: ItemId::new(format!("{n}-call")),
                output: "x".repeat(output_chars),
                is_error: false,
            },
        ),
        item(
            format!("{n}-reply"),
            n,
            ItemBody::AssistantMessage {
                text: format!("Reply {n}"),
            },
        ),
    ]
}

fn session(turns: usize, output_chars: usize) -> Vec<Item> {
    (1..=turns).flat_map(|n| turn(n, output_chars)).collect()
}

fn ids(items: &[Item]) -> Vec<&str> {
    items.iter().map(|item| item.id.as_str()).collect()
}

/// Every tool call has its result and every result its call.
fn assert_pairs_intact(items: &[Item]) {
    let calls: HashSet<&ItemId> = items
        .iter()
        .filter(|item| matches!(item.body, ItemBody::ToolCall { .. }))
        .map(|item| &item.id)
        .collect();
    let answered: HashSet<&ItemId> = items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::ToolResult { call_id, .. } => Some(call_id),
            _ => None,
        })
        .collect();
    assert_eq!(calls, answered);
}

#[test]
fn a_session_that_fits_passes_through_unchanged() {
    let items = session(3, 10_000);
    assert_eq!(transcript(items.clone(), estimate(&items)), items);
}

#[test]
fn a_200_turn_session_fits_keeping_the_opening_request_and_the_newest_turns() {
    let items = session(200, 6_000);
    let budget = 50_000;
    assert!(estimate(&items) > 5 * budget);

    let seed = transcript(items, budget);
    assert!(estimate(&seed) <= budget, "{} > {budget}", estimate(&seed));
    assert_eq!(
        seed[0].body,
        ItemBody::UserMessage {
            text: "Request 1".into(),
            attachments: Vec::new(),
        }
    );
    assert_eq!(
        seed[1].body,
        ItemBody::UserMessage {
            text: NOTE.into(),
            attachments: Vec::new()
        }
    );
    assert_eq!(
        ids(&seed[seed.len() - 4..]),
        ["200-user", "200-call", "200-result", "200-reply"]
    );
    assert!(seed.len() > 40, "kept only {} items", seed.len());
    assert!(!ids(&seed).contains(&"100-user"));
    assert!(
        !seed
            .iter()
            .any(|item| matches!(item.body, ItemBody::Reasoning { .. }))
    );
    assert_pairs_intact(&seed);
    // The newest turns keep their outputs whole; the older ones kept are cut.
    for item in &seed {
        if let ItemBody::ToolResult { output, .. } = &item.body {
            let newest = ["198-result", "199-result", "200-result"].contains(&item.id.as_str());
            assert_eq!(output.chars().count() == 6_000, newest, "{}", item.id);
            assert_eq!(output.contains("[… 3000 chars elided …]"), !newest);
        }
    }
}

fn output<'a>(seed: &'a [Item], id: &str) -> &'a str {
    match seed
        .iter()
        .find(|item| item.id.as_str() == id)
        .map(|item| &item.body)
    {
        Some(ItemBody::ToolResult { output, .. }) => output,
        other => panic!("expected tool result {id}, got {other:?}"),
    }
}

#[test]
fn cuts_go_to_the_oldest_outputs_first_and_stop_once_it_fits() {
    let items = session(10, 20_000);
    // Cutting five outputs is not enough, six is.
    let seed = transcript(items.clone(), 26_000);
    assert!(estimate(&seed) <= 26_000);
    // Every turn survives, minus its reasoning: nothing was dropped.
    assert_eq!(seed.len(), items.len() - 10);
    assert!(!ids(&seed).contains(&NOTE_ID));
    for n in 1..=6 {
        let cut = output(&seed, &format!("{n}-result"));
        assert!(cut.starts_with(&"x".repeat(KEEP_EACH_END)));
        assert!(cut.contains("[… 17000 chars elided …]"), "{n}");
        assert!(cut.ends_with(&"x".repeat(KEEP_EACH_END)));
    }
    // The newest turn's long output, and the others the cuts did not reach, stay whole.
    for n in 7..=10 {
        assert_eq!(output(&seed, &format!("{n}-result")).len(), 20_000, "{n}");
    }
}

#[test]
fn the_newest_turns_are_cut_only_when_they_alone_exceed_the_budget() {
    let seed = transcript(session(3, 20_000), 4_000);
    assert!(estimate(&seed) <= 4_000);
    for n in 1..=3 {
        assert!(output(&seed, &format!("{n}-result")).contains("[… 17000 chars elided …]"));
    }
}

#[test]
fn a_newest_turn_too_big_alone_loses_its_oldest_items_in_whole_tool_pairs() {
    let mut items = turn(1, 0);
    // A turn of 50 tool calls, each answered after the next one starts.
    let mut newest = vec![item(
        "2-user".into(),
        2,
        ItemBody::UserMessage {
            text: "Request 2".into(),
            attachments: Vec::new(),
        },
    )];
    for n in 0..50 {
        let call = |n: usize| {
            item(
                format!("call-{n}"),
                2,
                ItemBody::ToolCall {
                    name: "Read".into(),
                    input: json!({"path": "y".repeat(400)}),
                },
            )
        };
        if n == 0 {
            newest.push(call(0));
        }
        if n + 1 < 50 {
            newest.push(call(n + 1));
        }
        newest.push(item(
            format!("result-{n}"),
            2,
            ItemBody::ToolResult {
                call_id: ItemId::new(format!("call-{n}")),
                output: "z".repeat(400),
                is_error: false,
            },
        ));
    }
    items.extend(newest);

    let seed = transcript(items, 2_000);
    assert!(estimate(&seed) <= 2_000);
    assert_eq!(ids(&seed)[..2], ["1-user", NOTE_ID]);
    assert_eq!(ids(&seed).last(), Some(&"result-49"));
    assert!(!ids(&seed).contains(&"2-user"));
    assert_pairs_intact(&seed);
}

#[test]
fn budgets_follow_provider_and_model_family() {
    assert_eq!(budget(&Provider::Claude, ""), 100_000);
    assert_eq!(budget(&Provider::Claude, "claude-opus-5-5[1m]"), 400_000);
    assert_eq!(budget(&Provider::Codex, "gpt-6"), 100_000);
    assert_eq!(budget(&Provider::Other("fake".into()), ""), DEFAULT_BUDGET);
}
