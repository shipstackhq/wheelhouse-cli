//! Rust against Node: the runner's outputs must equal the daemon's, on the inputs in `rust/tools/gen-runner-golden.mjs`.
//! Needs `rust/fixtures/runner.json`, written by `node rust/tools/gen-runner-golden.mjs` (CI runs it first).

use serde_json::Value;
use std::path::PathBuf;
use wh_core::chat::{chat_args, chat_parser, chat_rules};
use wh_core::config::default_runner;
use wh_core::runner::{
    build_prompt, claude_args, events_from, is_rate_limited, scrub, tool_summary, SHELL_RULES,
};
use wh_core::workspace::ChatWorktree;

const HOME: &str = "/Users/example";

fn golden() -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/runner.json");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|_| {
        panic!(
            "missing {}: run `node rust/tools/gen-runner-golden.mjs`",
            p.display()
        )
    });
    serde_json::from_str(&text).unwrap()
}

fn str_vec(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn default_runner_policy_matches_node() {
    assert_eq!(default_runner().to_json(), golden()["defaultRunner"]);
}

#[test]
fn shell_rules_match_node() {
    assert_eq!(SHELL_RULES, golden()["shellRules"].as_str().unwrap());
}

#[test]
fn prompts_match_node() {
    let g = golden();
    for (name, want) in g["prompts"].as_object().unwrap() {
        let offer = &g["offers"][name];
        assert_eq!(
            build_prompt(offer),
            want.as_str().unwrap(),
            "prompt for {name}"
        );
    }
}

#[test]
fn claude_arguments_match_node() {
    let g = golden();
    let runner = default_runner();
    let mcp = Some("/tmp/wh-mcp/mcp.json");
    let mut restricted = runner.clone();
    restricted.disallowed_tools = vec![];
    restricted.allowed_tools = vec!["Bash(ls:*)".into()];
    let cases = [
        ("plain", claude_args(&g["offers"]["plain"], &runner, None)),
        (
            "effortAndAgent",
            claude_args(&g["offers"]["agent"], &runner, None),
        ),
        (
            "mcp",
            claude_args(&g["offers"]["mcpReadOnly"], &runner, mcp),
        ),
        ("review", claude_args(&g["offers"]["review"], &runner, mcp)),
        (
            "reviewNoMcp",
            claude_args(&g["offers"]["reviewNoPr"], &runner, None),
        ),
        (
            "restrictedRunner",
            claude_args(&g["offers"]["plain"], &restricted, None),
        ),
    ];
    for (name, got) in cases {
        assert_eq!(got, str_vec(&g["args"][name]), "arguments for {name}");
    }
}

#[test]
fn scrub_matches_node() {
    for c in golden()["scrub"].as_array().unwrap() {
        assert_eq!(
            scrub(c["input"].as_str().unwrap(), HOME),
            c["output"].as_str().unwrap(),
            "scrub {}",
            c["name"]
        );
    }
}

#[test]
fn tool_summaries_match_node() {
    for c in golden()["toolSummary"].as_array().unwrap() {
        let got = tool_summary(c["name"].as_str().unwrap(), &c["input"], HOME);
        assert_eq!(
            got,
            c["output"].as_str().unwrap(),
            "tool summary {}",
            c["name"]
        );
    }
}

#[test]
fn stream_lines_match_node() {
    for c in golden()["eventsFrom"].as_array().unwrap() {
        let got: Vec<Value> = events_from(c["line"].as_str().unwrap(), HOME)
            .into_iter()
            .map(|e| serde_json::json!({"type": e.kind, "data": e.data}))
            .collect();
        assert_eq!(Value::Array(got), c["events"], "line {}", c["line"]);
    }
}

#[test]
fn rate_limit_detection_matches_node() {
    for c in golden()["isRateLimited"].as_array().unwrap() {
        assert_eq!(
            is_rate_limited(c["input"].as_str().unwrap()),
            c["output"].as_bool().unwrap(),
            "{}",
            c["input"]
        );
    }
}

fn checkout(v: &Value) -> ChatWorktree {
    ChatWorktree {
        dir: std::path::PathBuf::new(),
        commit: v["commit"].as_str().unwrap().into(),
        branches: str_vec(&v["branches"]),
    }
}

#[test]
fn chat_arguments_match_node() {
    let g = golden();
    let c = &g["chat"];
    let at = checkout(&c["checkout"]);
    let msgs = &c["messages"];
    for name in ["plain", "resumed", "read", "write", "noRepoFullName"] {
        assert_eq!(
            chat_args(&msgs[name], &at, None),
            str_vec(&c["args"][name]),
            "chat args for {name}"
        );
    }
    let mcp = Some("/tmp/wh-mcp/mcp.json");
    assert_eq!(
        chat_args(&msgs["read"], &at, mcp),
        str_vec(&c["args"]["withConfig"])
    );
    assert_eq!(
        chat_args(&msgs["write"], &at, mcp),
        str_vec(&c["args"]["writeWithConfig"])
    );
    assert_eq!(
        chat_args(&msgs["plain"], &checkout(&c["checkoutBare"]), None),
        str_vec(&c["args"]["bareCheckout"])
    );
}

#[test]
fn chat_rules_match_node() {
    let g = golden();
    let c = &g["chat"];
    let at = checkout(&c["checkout"]);
    let repo = &c["messages"]["plain"]["repository"];
    for (name, wh, write) in [
        ("none", false, false),
        ("read", true, false),
        ("write", true, true),
    ] {
        assert_eq!(
            chat_rules(repo, &at, wh, write),
            c["rules"][name].as_str().unwrap(),
            "chat rules {name}"
        );
    }
}

#[test]
fn chat_parser_matches_node() {
    let g = golden();
    let p = &g["chat"]["parser"];
    let mut parse = chat_parser(HOME.into());
    for (line, want) in p["lines"]
        .as_array()
        .unwrap()
        .iter()
        .zip(p["events"].as_array().unwrap())
    {
        let got: Vec<Value> = parse(line.as_str().unwrap())
            .into_iter()
            .map(|e| serde_json::json!({ "type": e.kind, "data": e.data }))
            .collect();
        assert_eq!(&Value::Array(got), want, "events for {line}");
    }
}
