use super::*;

#[test]
fn attribution_headers_all_present() {
    let h = attribution_headers();
    assert_eq!(
        h["HTTP-Referer"],
        "https://github.com/navneetankur/chatport"
    );
    assert_eq!(h["X-OpenRouter-Title"], "chatport");
    assert_eq!(h["X-OpenRouter-App-Visibility"], "hidden");
    assert_eq!(h["X-OpenRouter-Categories"], "general-chat");
}

fn req(model: &str, cache: CachePolicy) -> ChatRequest {
    ChatRequest {
        key: String::new(),
        model: model.into(),
        system: Some("sys".into()),
        messages: vec![
            ChatMessage::text(Role::User, "a".into()),
            ChatMessage::text(Role::Assistant, "b".into()),
            ChatMessage::text(Role::User, "c".into()),
        ],
        options: Some(json!({ "order": ["anthropic"] })),
        cache,
        reasoning_effort: None,
        tools: vec![],
        web_tools: false,
        session_id: "conv1".into(),
    }
}

#[test]
fn session_id_always_sent() {
    assert_eq!(
        build_body(&req("x/y", CachePolicy::Off))["session_id"],
        "conv1"
    );
}

#[test]
fn anthropic_gets_markers_on_system_and_last_user() {
    let b = build_body(&req("anthropic/claude-x", CachePolicy::Auto));
    let m = b["messages"].as_array().unwrap();
    assert!(m[0]["content"][0]["cache_control"].is_object());
    assert!(m[1]["content"].is_string());
    assert!(m[3]["content"][0]["cache_control"].is_object());
    assert_eq!(b["provider"]["order"][0], "anthropic");
}

#[test]
fn reasoning_effort_only_when_set() {
    let mut r = req("x/y", CachePolicy::Off);
    assert!(build_body(&r).get("reasoning").is_none());
    r.reasoning_effort = Some(ReasoningEffort::Xhigh);
    assert_eq!(build_body(&r)["reasoning"], json!({ "effort": "xhigh" }));
}

#[test]
fn no_markers_when_off_or_auto_model() {
    for b in [
        build_body(&req("anthropic/claude-x", CachePolicy::Off)),
        build_body(&req("openai/gpt-x", CachePolicy::Auto)),
        build_body(&req("google/gemini-x", CachePolicy::Auto)),
    ] {
        assert!(
            b["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["content"].is_string())
        );
    }
}

#[test]
fn parses_delta_usage_done_error() {
    let ev = parse_chunk(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).unwrap();
    assert!(matches!(&ev[0], ChatEvent::Delta(t) if t == "hi"));
    let ev = parse_chunk(
        r#"{"model":"x/y","provider":"Nvidia","choices":[{"delta":{"content":"hi"}}]}"#,
    )
    .unwrap();
    assert!(matches!(&ev[0], ChatEvent::Model(m) if m == "x/y"));
    assert!(matches!(&ev[1], ChatEvent::Provider(p) if p == "Nvidia"));
    assert!(matches!(&ev[2], ChatEvent::Delta(t) if t == "hi"));
    let ev = parse_chunk(r#"{"choices":[{"delta":{"content":"","reasoning":"hmm"}}]}"#).unwrap();
    assert!(matches!(&ev[..], [ChatEvent::Reasoning(t)] if t == "hmm"));
    let ev = parse_chunk(
        r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":8},"completion_tokens_details":{"reasoning_tokens":1},"cost":0.01,"cost_details":{"upstream_inference_cost":0.5}}}"#,
    )
    .unwrap();
    assert!(
        matches!(&ev[0], ChatEvent::Usage(u) if u.cached_tokens == 8 && u.reasoning_tokens == 1 && u.cost == Some(0.01) && u.upstream_cost == Some(0.5))
    );
    assert!(matches!(parse_chunk("[DONE]").unwrap()[0], ChatEvent::Done));
    assert!(parse_chunk(r#"{"error":{"message":"nope"}}"#).is_err());
}

#[test]
fn tools_and_tool_messages_in_body() {
    let mut r = req("x/y", CachePolicy::Off);
    assert!(build_body(&r).get("tools").is_none());
    r.tools = crate::tools::specs();
    r.messages.push(ChatMessage {
        role: Role::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "t1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a"}"#.into(),
        }],
        tool_call_id: None,
    });
    r.messages.push(ChatMessage {
        role: Role::Tool,
        content: "hello".into(),
        tool_calls: vec![],
        tool_call_id: Some("t1".into()),
    });
    let b = build_body(&r);
    assert_eq!(b["tools"][1]["function"]["name"], "read_file");
    let m = b["messages"].as_array().unwrap();
    let call = &m[4];
    assert!(call["content"].is_null());
    assert_eq!(
        call["tool_calls"][0]["function"]["arguments"],
        r#"{"path":"a"}"#
    );
    assert_eq!(
        m[5],
        json!({ "role": "tool", "content": "hello", "tool_call_id": "t1" })
    );
}

#[test]
fn marker_moves_to_last_tool_result() {
    let mut r = req("anthropic/claude-x", CachePolicy::Auto);
    r.messages.push(ChatMessage {
        role: Role::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: "t1".into(),
            name: "list_dir".into(),
            arguments: "{}".into(),
        }],
        tool_call_id: None,
    });
    r.messages.push(ChatMessage {
        role: Role::Tool,
        content: "x".into(),
        tool_calls: vec![],
        tool_call_id: Some("t1".into()),
    });
    let b = build_body(&r);
    let m = b["messages"].as_array().unwrap();
    assert!(m[3]["content"].is_string(), "user msg no longer marked");
    assert!(m[4]["content"].is_null());
    assert_eq!(m[5]["content"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(m[5]["tool_call_id"], "t1");
}

#[test]
fn parses_tool_call_fragments() {
    let ev = parse_chunk(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
    )
    .unwrap();
    assert!(
        matches!(&ev[..], [ChatEvent::ToolCall { index: 0, id: Some(i), name: Some(n), arguments }]
        if i == "t1" && n == "read_file" && arguments.is_empty())
    );
    let ev = parse_chunk(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"pa"}}]}}]}"#,
    )
    .unwrap();
    assert!(
        matches!(&ev[..], [ChatEvent::ToolCall { index: 1, id: None, name: None, arguments }]
        if arguments == "{\"pa")
    );
}

#[test]
fn batch_body_fields_before_requests() {
    let mut r = req("anthropic/claude-x:batch", CachePolicy::Auto);
    r.options = Some(json!({ "only": ["anthropic"], "data_collection": "deny" }));
    let b = build_batch_body(&r);
    let s = serde_json::to_string(&b).unwrap();
    let at = |k: &str| s.find(&format!("\"{k}\"")).unwrap();
    assert!(at("endpoint") < at("requests"));
    assert!(at("model") < at("requests"));
    assert!(at("provider") < at("requests"));
    assert_eq!(b["provider"], json!({ "only": ["anthropic"] }));
    let body = &b["requests"][0]["body"];
    assert!(body.get("stream").is_none() && body.get("provider").is_none());
    assert!(body.get("model").is_none());
    assert_eq!(body["session_id"], "conv1");
    assert!(body["messages"][0]["content"][0]["cache_control"].is_object());
    // Only unsupported keys → no provider at all.
    r.options = Some(json!({ "data_collection": "deny" }));
    assert!(build_batch_body(&r).get("provider").is_none());
}

#[test]
fn parse_batch_states() {
    let p = parse_batch(&json!({ "status": "in_progress", "results": null })).unwrap();
    assert!(matches!(p, BatchPoll::Pending(i) if i.status == "in_progress"));
    let p = parse_batch(
        &json!({ "status": "validating", "provider": "Google AI Studio", "expires_at": 9 }),
    )
    .unwrap();
    assert!(matches!(p, BatchPoll::Pending(i)
        if i.provider.as_deref() == Some("Google AI Studio") && i.expires_at == Some(9)));
    let p = parse_batch(&json!({ "status": "completed", "results": null })).unwrap();
    assert!(matches!(p, BatchPoll::Pending(i) if i.status == "finalizing"));
    let p = parse_batch(&json!({ "status": "expired", "error": { "message": "late" } })).unwrap();
    assert!(matches!(p, BatchPoll::Done(r) if r.error.as_deref() == Some("batch expired: late")));
    let p = parse_batch(&json!({ "status": "completed", "results": [
        { "custom_id": "r1", "response": null, "error": { "message": "too long" } }
    ]}))
    .unwrap();
    assert!(matches!(p, BatchPoll::Done(r) if r.error.as_deref() == Some("too long")));
    let p = parse_batch(
        &json!({ "status": "completed", "provider": "Anthropic", "results": [
            { "custom_id": "r1", "error": null, "response": { "status_code": 200, "body": {
                "model": "anthropic/claude-x",
                "choices": [{ "message": { "content": "hi", "reasoning": "hm",
                    "tool_calls": [{ "function": { "name": "read_file" } }] } }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 2 }
            }}}
        ], "usage": { "prompt_tokens": 5, "completion_tokens": 2, "cost": 0.01 }}),
    )
    .unwrap();
    let BatchPoll::Done(r) = p else { panic!() };
    assert_eq!((r.content.as_str(), r.reasoning.as_str()), ("hi", "hm"));
    assert_eq!(r.model.as_deref(), Some("anthropic/claude-x"));
    assert_eq!(r.provider.as_deref(), Some("Anthropic"));
    assert_eq!(r.usage.unwrap().cost, Some(0.01));
    assert_eq!(r.tool_calls, ["read_file"]);
    assert!(r.error.is_none());
}

#[test]
fn web_tools_in_body() {
    let mut r = req("x/y", CachePolicy::Off);
    r.web_tools = true;
    let b = build_body(&r);
    let types: Vec<&str> = b["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "openrouter:web_search",
            "openrouter:web_fetch",
            "openrouter:datetime"
        ]
    );
    assert_eq!(b["tools"][0]["parameters"]["engine"], "exa");
    assert_eq!(b["tools"][1]["parameters"]["engine"], "exa");

    // Alongside file tools: function tools first, server tools appended.
    r.tools = crate::tools::specs();
    let b = build_body(&r);
    let t = b["tools"].as_array().unwrap();
    assert_eq!(t.len(), crate::tools::specs().len() + 3);
    assert_eq!(t[0]["type"], "function");
    assert_eq!(t[t.len() - 1]["type"], "openrouter:datetime");
}

#[test]
fn web_searches_parsed_and_summed() {
    let ev = parse_chunk(
        r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":1,"server_tool_use_details":{"web_search_requests":2,"tool_calls_requested":3,"tool_calls_executed":3}}}"#,
    )
    .unwrap();
    let ChatEvent::Usage(u) = &ev[0] else {
        panic!("expected usage, got {ev:?}")
    };
    assert_eq!(u.web_searches, 2);
    let mut total = u.clone();
    total.add(u);
    assert_eq!(total.web_searches, 4);
    // Zero is left out of the stored JSON.
    let none = usage_from(&serde_json::json!({ "prompt_tokens": 1 }));
    assert!(
        serde_json::to_value(&none)
            .unwrap()
            .get("web_searches")
            .is_none()
    );
}

#[test]
fn raw_kept_only_for_non_text_chunks() {
    let raw = |d: &str| {
        parse_chunk(d)
            .unwrap()
            .iter()
            .any(|e| matches!(e, ChatEvent::Raw(_)))
    };
    assert!(!raw(
        r#"{"choices":[{"delta":{"role":"assistant","content":"hi"},"finish_reason":null}]}"#
    ));
    assert!(!raw(
        r#"{"choices":[{"delta":{"reasoning":"hmm","reasoning_details":[]}}]}"#
    ));
    assert!(raw(
        r#"{"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}"#
    ));
    assert!(raw(
        r#"{"choices":[{"delta":{"annotations":[{"type":"url_citation"}]}}]}"#
    ));
    assert!(raw(
        r#"{"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#
    ));
    assert!(!raw(r#"{"choices":[{"delta":{"tool_calls":[]}}]}"#));
    assert!(!raw("[DONE]"));
}
