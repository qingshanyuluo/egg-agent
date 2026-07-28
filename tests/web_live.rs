use egg_agent::tools::ToolRegistry;

#[tokio::test]
#[ignore] // network; run explicitly with --ignored
async fn web_fetch_live() {
    let reg = ToolRegistry::default_set();
    let out = reg
        .dispatch("web_fetch", r#"{"url":"https://example.com","format":"markdown"}"#)
        .await;
    println!("FETCH OUT:\n{out}");
    assert!(out.to_lowercase().contains("example"), "out: {out}");
}

#[tokio::test]
#[ignore]
async fn web_search_live() {
    let reg = ToolRegistry::default_set();
    let out = reg
        .dispatch("web_search", r#"{"query":"rust tokio tutorial","num_results":3}"#)
        .await;
    println!("SEARCH OUT:\n{out}");
}
