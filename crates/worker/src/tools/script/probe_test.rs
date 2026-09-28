#[tokio::test]
async fn long_script_is_promoted_to_async_operation_and_completes() {
    let tool = ScriptTool::with_initial_wait(
        vec![FakeTool::slow("mcp_test__slow", Duration::from_millis(800))],
        vec![],
        None,
        Duration::from_millis(200),
    );
    let runtime = AsyncOperationRuntime::new(AsyncOpManager::new());
    let result = scope_async_operation_runtime(runtime.clone(), async {
        tool.execute(json!({"script": r#"
                ctx.log("started");
                return await ctx.mcp.mcp_test__slow({ n: 1 });
            "#}))
            .await
            .expect("execute should not error at the transport level")
    })
    .await;
    let receipt: Value = serde_json::from_str(&result.output.text_content()).unwrap();
    let operation_id = receipt["operation_id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let inspect = runtime.inspect(vec![operation_id], None, true, 10).await;
    let printed = serde_json::to_value(&inspect).unwrap().to_string();
    let idx = printed.find("content").expect("content present");
    let slice = &printed[idx - 2..idx + 30];
    eprintln!("SLICE BYTES: {:?}", slice);
    assert!(printed.contains(r#"\"content\":{\"n\":1.0}"#) || printed.contains("\"content\":{\"n\":1.0}"), "missing");
}
