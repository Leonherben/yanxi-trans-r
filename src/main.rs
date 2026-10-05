use clap::Parser;
use colored::*;
use std::io::{self, BufRead, IsTerminal, Write};
use yanxi_trans_r::cache::SQLiteCache;
use yanxi_trans_r::config::AppConfig;
use yanxi_trans_r::models::TranslationRequest;
use yanxi_trans_r::translator::create_translator;

#[derive(Parser, Debug)]
#[command(
    name = "yanxi-cli",
    about = "言蹊翻译 - 极简极速跨平台划词翻译核心终端工具 (Rust)",
    version = "0.1.0"
)]
struct Cli {
    /// 待翻译文本（支持传入多个词或通过管道输入）
    #[arg(trailing_var_arg = true)]
    text: Vec<String>,

    /// 源语言代码（默认: auto）
    #[arg(short = 's', long = "source")]
    source: Option<String>,

    /// 目标语言代码（默认: zh-CN）
    #[arg(short = 't', long = "target")]
    target: Option<String>,

    /// 指定翻译服务提供商 (如 microsoft, deepseek, openai)
    #[arg(short = 'p', long = "provider")]
    provider: Option<String>,

    /// 设为默认翻译提供商
    #[arg(long = "set-provider", value_name = "PROVIDER")]
    set_provider: Option<String>,

    /// 设置指定 Provider 的 API 密钥: --set-key <PROVIDER> <KEY>
    #[arg(long = "set-key", num_args = 2, value_names = ["PROVIDER", "KEY"])]
    set_key: Option<Vec<String>>,

    /// 列出所有已配置的翻译服务提供商
    #[arg(long = "list-providers")]
    list_providers: bool,

    /// 测试指定或当前提供商的连通性
    #[arg(long = "test")]
    test: bool,

    /// 清空本地翻译缓存数据库
    #[arg(long = "clear-cache")]
    clear_cache: bool,

    /// 进入终端交互式翻译 REPL 模式
    #[arg(short = 'i', long = "interactive")]
    interactive: bool,

    /// 禁用 SQLite 缓存直接调用接口
    #[arg(long = "no-cache")]
    no_cache: bool,

    /// 启动桌面全局划词监听守护模式
    #[arg(short = 'w', long = "watch")]
    watch: bool,

    /// 以 JSON 格式输出结果
    #[arg(long = "json")]
    json: bool,
}

async fn do_translate(
    text: &str,
    source: &str,
    target: &str,
    provider_name: &str,
    config: &AppConfig,
    cache: &SQLiteCache,
    no_cache: bool,
    json_output: bool,
) {
    let clean = text.trim();
    if clean.is_empty() {
        return;
    }

    let provider_cfg = match config.providers.get(provider_name) {
        Some(cfg) => cfg,
        None => {
            eprintln!(
                "{} 找不到提供商配置: {}",
                "❌".red().bold(),
                provider_name.yellow()
            );
            return;
        }
    };

    let req = TranslationRequest::with_langs(clean, source, target);

    // 1. 尝试从缓存读取
    if !no_cache {
        if let Some(cached) = cache.get(&req, provider_name) {
            if json_output {
                let _ = serde_json::to_writer_pretty(io::stdout(), &cached);
                println!();
            } else {
                println!(
                    "{} {} ({})",
                    "⚡ [Cache]".yellow().bold(),
                    format!("[{}]", cached.provider).cyan(),
                    format!("{} -> {}", cached.source_lang, cached.target_lang).dimmed()
                );
                println!("{} {}", "👉".bold(), cached.translated_text.green().bold());
                if let Some(ref ph) = cached.phonetic {
                    println!("   {} {}", "拼音/注音:".dimmed(), ph.yellow());
                }
                println!();
            }
            return;
        }
    }

    // 2. 调用 API 翻译
    let translator = create_translator(provider_cfg);
    if !json_output {
        print!(
            "{} 正在通过 [{}] 请求翻译...",
            "⏳".cyan(),
            provider_name.cyan().bold()
        );
        let _ = io::stdout().flush();
    }

    let res = match translator.translate(&req).await {
        Ok(r) => r,
        Err(e) => {
            if json_output {
                let err_res = yanxi_trans_r::models::TranslationResult::error(
                    clean,
                    e.to_string(),
                    provider_name,
                );
                let _ = serde_json::to_writer_pretty(io::stdout(), &err_res);
                println!();
            } else {
                print!("\r{}\r", " ".repeat(50));
                eprintln!(
                    "{} 翻译异常 [{}]: {}\n",
                    "❌".red().bold(),
                    provider_name.yellow(),
                    e.to_string().red()
                );
            }
            return;
        }
    };

    if json_output {
        let _ = serde_json::to_writer_pretty(io::stdout(), &res);
        println!();
    } else {
        // 清除正在翻译的提示
        print!("\r{}\r", " ".repeat(60));
        if res.is_success() {
            let lang_info = if let Some(ref d) = res.detected_lang {
                format!("{d} (自动识别) -> {}", res.target_lang)
            } else {
                format!("{} -> {}", res.source_lang, res.target_lang)
            };

            println!(
                "{} {} ({})",
                "✅".green().bold(),
                format!("[{} | {:.1}ms]", res.provider, res.latency_ms).cyan().bold(),
                lang_info.dimmed()
            );
            println!("{} {}", "👉".bold(), res.translated_text.green().bold());
            if let Some(ref ph) = res.phonetic {
                println!("   {} {}", "拼音/注音:".dimmed(), ph.yellow());
            }
            println!();

            // 存入缓存
            if !no_cache {
                cache.put(&res);
            }
        } else {
            eprintln!(
                "{} 翻译失败 [{}]: {}\n",
                "❌".red().bold(),
                res.provider.yellow(),
                res.translated_text.red()
            );
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    let mut config = AppConfig::load();
    let cache = SQLiteCache::new()?;

    // 0. 清空缓存: --clear-cache
    if cli.clear_cache {
        cache.clear()?;
        println!("{} 本地翻译缓存数据库已清空", "✅".green().bold());
        return Ok(());
    }

    // 1. 设置默认 Provider
    if let Some(ref p_name) = cli.set_provider {
        if config.providers.contains_key(p_name) {
            config.active_provider = p_name.clone();
            config.save()?;
            println!(
                "{} 已将默认提供商切换为: {}",
                "✅".green().bold(),
                p_name.cyan().bold()
            );
        } else {
            eprintln!(
                "{} 提供商 [{}] 不存在，可用列表: {:?}",
                "❌".red().bold(),
                p_name.yellow(),
                config.providers.keys().collect::<Vec<_>>()
            );
        }
        return Ok(());
    }

    // 2. 设置 API Key: --set-key <PROVIDER> <KEY>
    if let Some(ref kv) = cli.set_key {
        if kv.len() == 2 {
            let p_name = &kv[0];
            let key = &kv[1];
            if let Some(provider) = config.providers.get_mut(p_name) {
                provider.api_key = key.clone();
                config.save()?;
                println!(
                    "{} 已成功更新 [{}] 的 API 密钥",
                    "✅".green().bold(),
                    p_name.cyan().bold()
                );
            } else {
                eprintln!(
                    "{} 提供商 [{}] 不存在，可用提供商: {:?}",
                    "❌".red().bold(),
                    p_name.yellow(),
                    config.providers.keys().collect::<Vec<_>>()
                );
            }
            return Ok(());
        }
    }

    // 3. 列出提供商: --list-providers
    if cli.list_providers {
        println!("\n{}", "=== 言蹊翻译 - 服务提供商列表 ===".bold().cyan());
        for (name, p) in &config.providers {
            let mark = if name == &config.active_provider {
                "★ (当前激活)".yellow().bold()
            } else {
                "             ".normal()
            };

            let key_status = if p.provider_type == "microsoft" {
                "✔ 官方免配置 (网页)".green()
            } else if !p.api_key.trim().is_empty() {
                "✔ 已配置 Key".green()
            } else {
                "✖ 未配置 Key".red()
            };

            println!(
                "{} {:<12} | 模型: {:<16} | {:<16} | {}",
                mark,
                name.bold(),
                p.model.cyan(),
                key_status,
                p.base_url.dimmed()
            );
        }
        println!();
        return Ok(());
    }

    let active_provider = cli
        .provider
        .clone()
        .unwrap_or_else(|| config.active_provider.clone());
    let source_lang = cli.source.clone().unwrap_or_else(|| "auto".to_string());
    let target_lang = cli
        .target
        .clone()
        .unwrap_or_else(|| config.target_lang.clone());

    // 4. 测试连通性: --test
    if cli.test {
        let provider_cfg = match config.providers.get(&active_provider) {
            Some(cfg) => cfg,
            None => {
                eprintln!(
                    "{} 提供商 [{}] 未配置",
                    "❌".red().bold(),
                    active_provider.yellow()
                );
                return Ok(());
            }
        };

        println!(
            "{} 正在测试 [{}] ({}) 连通性...",
            "🔍".cyan(),
            active_provider.cyan().bold(),
            provider_cfg.model.dimmed()
        );

        let translator = create_translator(provider_cfg);
        let (ok, msg) = translator.test_connection().await;
        if ok {
            println!("{} {}", "✅".green().bold(), msg.green());
        } else {
            eprintln!("{} {}", "❌".red().bold(), msg.red());
        }
        return Ok(());
    }

    // 4.5. 划词监听守护模式: --watch
    if cli.watch {
        println!(
            "\n{}",
            format!(
                "=== 言蹊翻译 桌面划词监听守护模式 [Provider: {}, {} -> {}] ===",
                active_provider.cyan().bold(),
                source_lang.yellow(),
                target_lang.yellow()
            )
            .bold()
        );
        println!(
            "{}",
            "🚀 后台划词监听器已启动 (X11 XFixes 零轮询驱动)，在任意窗口划选文字即可自动翻译 (按 Ctrl+C 退出)：\n".green()
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let listener = yanxi_trans_r::selection::create_selection_listener(
            config.selection.debounce_ms,
            1,
            3000,
        );
        listener.start(tx)?;

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n👋 收到退出信号，正在停止划词监听器...");
                listener.stop();
            }
            _ = async {
                while let Some(event) = rx.recv().await {
                    match event {
                        yanxi_trans_r::selection::SelectionEvent::Selected { text, pos } => {
                            println!(
                                "{} 原文: {}",
                                format!("🎯 [划词触发 @ ({}, {})]", pos.0, pos.1).magenta().bold(),
                                format!("\"{}\"", text).yellow()
                            );
                            do_translate(
                                &text,
                                &source_lang,
                                &target_lang,
                                &active_provider,
                                &config,
                                &cache,
                                cli.no_cache,
                                cli.json,
                            ).await;
                        }
                        yanxi_trans_r::selection::SelectionEvent::Cleared { .. } => {
                            // 选区清空通知 (供 Phase 3 悬浮弹窗收起联动)
                        }
                    }
                }
            } => {}
        }

        println!("{} 划词监听守护进程已退出", "✅".green().bold());
        return Ok(());
    }

    // 5. 交互模式: --interactive
    if cli.interactive {
        println!(
            "\n{}",
            format!(
                "=== 言蹊翻译 交互模式 [Provider: {}, {} -> {}] ===",
                active_provider.cyan().bold(),
                source_lang.yellow(),
                target_lang.yellow()
            )
            .bold()
        );
        println!(
            "{}",
            "输入待翻译内容后回车，输入 'exit' 或按 Ctrl+C 退出：\n".dimmed()
        );

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        loop {
            print!("{}", "yanxi> ".bold().blue());
            let _ = stdout.flush();

            let mut line = String::new();
            if stdin.read_line(&mut line)? == 0 {
                break;
            }

            let input = line.trim();
            if input.is_empty() {
                continue;
            }
            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                break;
            }

            do_translate(
                input,
                &source_lang,
                &target_lang,
                &active_provider,
                &config,
                &cache,
                cli.no_cache,
                cli.json,
            )
            .await;
        }
        println!("\n👋 已退出交互模式");
        return Ok(());
    }

    // 6. 单次翻译：参数或标准输入
    let text = if !cli.text.is_empty() {
        cli.text.join(" ")
    } else if !io::stdin().is_terminal() {
        let mut buffer = String::new();
        io::stdin().lock().read_line(&mut buffer)?;
        buffer.trim().to_string()
    } else {
        String::new()
    };

    if !text.is_empty() {
        do_translate(
            &text,
            &source_lang,
            &target_lang,
            &active_provider,
            &config,
            &cache,
            cli.no_cache,
            cli.json,
        )
        .await;
    } else {
        println!(
            "💡 {}: 请输入待翻译的文本，或运行 `{}` 查看帮助。\n示例: `{}`",
            "提示".yellow().bold(),
            "yanxi-cli --help".cyan(),
            "yanxi-cli \"hello world\"".green()
        );
    }

    Ok(())
}
