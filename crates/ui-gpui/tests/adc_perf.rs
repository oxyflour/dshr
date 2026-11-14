//! ADC-v1 渲染预算探针（**无窗口**，CI 可跑）。
//!
//! 测的是 GPUI 元素树的**构造**成本：`parse_tree` + `adc_view`。真正的
//! layout/prepaint/paint 需要 `gpui::Window`；本仓离线环境没有启用 gpui 的
//! `test-support` feature（会拉 proptest/reqwest 等 dev 依赖），所以窗口内的
//! draw 探针明确留给 M4 / 手动执行（方法见 `docs/m1-log.md` A1c：profiler +
//! `FrameTiming` 分位数）。**本文件不测量 draw 时间**，只给出上界参考：
//! 构造阶段的 p95 必须远小于 16.7 ms 帧预算，否则窗口内没有余量。
//!
//! 覆盖 10 棵典型 ADC 树（会话卡片 / 工具卡 / 表格 / 设置面板 / 列表 / 树 /
//! 嵌套布局 / 图表 / Markdown 8KB / Code 400 行），每棵重复 [`ROUNDS`] 次取
//! p50/p95，并断言上界 [`P95_LIMIT_MS`]。
//!
//! 运行：`cargo test --offline -p dshr-ui-gpui --test adc_perf -- --nocapture`
//! 输出：逐树表格 + 一行 `ADC-PERF-JSON {...}`（供 docs/m3-render-budget.md 取数）。

use std::hint::black_box;
use std::time::Instant;

use dshr_ui_contributions::{parse_tree, ElementNode};
use dshr_ui_gpui::{adc_view, Theme};
use serde_json::{json, Value};

/// 每棵树丢弃的预热轮数（分配器/分支预测稳定化）。
const WARMUP: usize = 5;

/// 每棵树的采样轮数；60 轮足以让 p95 落在第 57 个样本上。
const ROUNDS: usize = 60;

/// 构造阶段 p95 上限（ms）。本机 debug 基线远低于该值；超限说明渲染器退化。
const P95_LIMIT_MS: f64 = 50.0;

/// 一棵典型 ADC 树。
struct Fixture {
    /// 树名（报告用）。
    name: &'static str,
    /// ADC-v1 JSON 元素树。
    tree: Value,
    /// 数据行数（表格单元格/列表项/树节点/代码行/Markdown 块），用于解释构造成本。
    data_rows: usize,
}

// ---------------------------------------------------------------------------
// 树构造小工具
// ---------------------------------------------------------------------------

fn text(value: &str) -> Value {
    json!({"type": "Text", "props": {"value": value}})
}

fn label(value: &str) -> Value {
    json!({"type": "Text", "props": {"value": value, "style": "caption"}})
}

fn stack(children: Vec<Value>) -> Value {
    json!({"type": "Stack", "props": {"gap": 8}, "children": children})
}

fn row(children: Vec<Value>) -> Value {
    json!({"type": "Row", "props": {"gap": 8, "align": "center"}, "children": children})
}

fn grid(columns: usize, children: Vec<Value>) -> Value {
    json!({"type": "Grid", "props": {"columns": columns, "gap": 8}, "children": children})
}

fn card(title: &str, children: Vec<Value>) -> Value {
    json!({"type": "Card", "props": {"title": title}, "children": children})
}

fn badge(value: &str, tone: &str) -> Value {
    json!({"type": "Badge", "props": {"label": value, "tone": tone}})
}

fn button(value: &str) -> Value {
    json!({"type": "Button", "props": {"label": value, "a11yLabel": value}})
}

fn icon(name: &str) -> Value {
    json!({"type": "Icon", "props": {"name": name, "a11yLabel": name}})
}

fn input(value: &str) -> Value {
    json!({"type": "Input", "props": {"value": value, "a11yLabel": "输入"}})
}

fn select(value: &str) -> Value {
    json!({
        "type": "Select",
        "props": {"value": value, "options": [{"value": "a", "label": "A"}], "a11yLabel": "选择"}
    })
}

fn divider() -> Value {
    json!({"type": "Divider"})
}

fn spacer() -> Value {
    json!({"type": "Spacer"})
}

fn key_value(pairs: &[(&str, &str)]) -> Value {
    let pairs: Vec<Value> = pairs
        .iter()
        .map(|(key, value)| json!({"key": key, "value": value}))
        .collect();
    json!({"type": "KeyValue", "props": {"pairs": pairs}})
}

fn table(columns: usize, rows: usize) -> Value {
    let cols: Vec<Value> = (0..columns)
        .map(|index| json!({"key": format!("c{index}"), "title": format!("列{index}")}))
        .collect();
    let row_values: Vec<Value> = (0..rows)
        .map(|row_index| {
            let mut cells = serde_json::Map::new();
            for column in 0..columns {
                cells.insert(format!("c{column}"), json!(row_index * columns + column));
            }
            Value::Object(cells)
        })
        .collect();
    json!({
        "type": "Table",
        "props": {"columns": cols, "rows": row_values, "a11yLabel": "明细表"}
    })
}

fn list(items: usize) -> Value {
    let values: Vec<Value> = (0..items)
        .map(|index| json!({"key": format!("k{index}"), "primary": format!("项 {index}"), "secondary": "详情"}))
        .collect();
    json!({"type": "List", "props": {"items": values, "a11yLabel": "列表"}})
}

fn tree(nodes: usize) -> Value {
    let leaves: Vec<Value> = (0..nodes)
        .map(|index| json!({"key": format!("leaf{index}"), "label": format!("叶 {index}")}))
        .collect();
    json!({
        "type": "Tree",
        "props": {
            "nodes": [{"key": "root", "label": "根", "children": leaves}],
            "expanded": ["root"],
            "a11yLabel": "树"
        }
    })
}

fn code_block(lines: usize) -> Value {
    // 每行约 15 字节：400 行仍在 8 KB 文本配额内。
    let source: String = (0..lines)
        .map(|index| format!("v{index} = f({index});\n"))
        .collect();
    json!({
        "type": "Code",
        "props": {"source": source, "language": "rust", "lineNumbers": true}
    })
}

/// 约 8 KB 的 Markdown（贴近插件一次提交的文本上限）。
fn markdown_8kb() -> Value {
    let block =
        "## 小节\n\n- 第一项，包含**强调**与 `code`\n- 第二项，[链接](https://example.com)\n\n";
    let mut source = String::new();
    while source.len() + block.len() <= 8192 {
        source.push_str(block);
    }
    json!({"type": "Markdown", "props": {"source": source}})
}

fn chart(kind: &str) -> Value {
    json!({
        "type": "Chart",
        "props": {
            "kind": kind,
            "data": [
                {"label": "一", "value": 3},
                {"label": "二", "value": 5},
                {"label": "三", "value": 2},
                {"label": "四", "value": 8},
                {"label": "五", "value": 6},
                {"label": "六", "value": 1},
                {"label": "七", "value": 4},
                {"label": "八", "value": 7}
            ]
        }
    })
}

/// 深度 8 的嵌套布局：每层 1 个容器 + 4 个旁注节点，最内层是叶子文本。
fn nested(depth: usize) -> Value {
    if depth == 0 {
        return text("leaf");
    }
    let inner = nested(depth - 1);
    let children = vec![
        inner,
        text("旁注"),
        label("补充说明"),
        badge("层级", "info"),
        text("备注"),
    ];
    match depth % 3 {
        0 => grid(2, children),
        1 => row(children),
        _ => stack(children),
    }
}

/// `cells` 个 `Stack(Text, label)` 组成的网格单元（面板里常见的说明块）。
fn cells(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| stack(vec![text(&format!("说明 {index}")), label("次级说明")]))
        .collect()
}

// ---------------------------------------------------------------------------
// 10 棵典型树
// ---------------------------------------------------------------------------

fn fixtures() -> Vec<Fixture> {
    let mut fixtures = Vec::with_capacity(10);

    // 1. 会话卡片：头部 + Markdown 正文 + 元信息 + 网格说明块 + 操作按钮。
    fixtures.push(Fixture {
        name: "chat-card",
        tree: card(
            "会话",
            vec![stack(vec![
                row(vec![
                    icon("check"),
                    stack(vec![text("会话标题"), label("12:00 · deepseek")]),
                ]),
                json!({"type": "Markdown", "props": {"source": "## 摘要\n\n- 一\n- 二"}}),
                row(vec![
                    badge("完成", "success"),
                    badge("2 步", "info"),
                    spacer(),
                    label("12:00"),
                ]),
                divider(),
                grid(2, cells(5)),
                row(vec![button("继续"), button("复制")]),
            ])],
        ),
        data_rows: 5,
    });

    // 2. 工具卡：图标 + 摘要 + 40 行代码 + 键值 + 列表 + 网格。
    fixtures.push(Fixture {
        name: "tool-card",
        tree: card(
            "工具调用",
            vec![stack(vec![
                row(vec![
                    icon("check"),
                    text("cad.validate"),
                    badge("通过", "success"),
                ]),
                text("校验通过：12 个实体，0 个错误"),
                code_block(40),
                divider(),
                row(vec![button("重试"), button("复制")]),
                key_value(&[("耗时", "120 ms"), ("版本", "0.2.2")]),
                list(3),
                grid(2, cells(5)),
            ])],
        ),
        data_rows: 40 + 3 + 4,
    });

    // 3. 设置面板：8 个设置行，控件混排。
    let controls = [
        json!({"type": "Switch", "props": {"checked": true, "label": "启用", "a11yLabel": "启用"}}),
        json!({"type": "Checkbox", "props": {"checked": false, "label": "合并", "a11yLabel": "合并"}}),
        json!({"type": "Slider", "props": {"value": 30, "min": 0, "max": 100, "a11yLabel": "比例"}}),
        select("a"),
    ];
    let settings: Vec<Value> = (0..8)
        .map(|index| {
            row(vec![
                stack(vec![
                    text(&format!("设置项 {index}")),
                    label("该项的说明文字"),
                ]),
                controls[index % controls.len()].clone(),
            ])
        })
        .collect();
    fixtures.push(Fixture {
        name: "settings-panel",
        tree: stack(vec![text("设置"), divider(), stack(settings)]),
        data_rows: 8,
    });

    // 4. 表格面板：6 个筛选行 + 12 列 × 200 行表格（配额上限）。
    fixtures.push(Fixture {
        name: "table-panel",
        tree: card(
            "明细",
            vec![stack(vec![
                row(vec![text("明细"), spacer(), badge("实时", "info")]),
                grid(2, {
                    (0..6)
                        .map(|index| row(vec![label(&format!("筛选 {index}")), input("值")]))
                        .collect()
                }),
                table(12, 200),
                divider(),
                row(vec![
                    button("导出"),
                    button("刷新"),
                    spacer(),
                    label("200 行"),
                ]),
            ])],
        ),
        data_rows: 200 * 12,
    });

    // 5. 列表面板：6 个筛选行 + 200 项列表。
    fixtures.push(Fixture {
        name: "list-panel",
        tree: card(
            "列表",
            vec![stack(vec![
                row(vec![text("列表"), spacer(), badge("200", "info")]),
                grid(2, {
                    (0..6)
                        .map(|index| row(vec![label(&format!("筛选 {index}")), input("值")]))
                        .collect()
                }),
                list(200),
                divider(),
                row(vec![input("搜索"), button("确定")]),
            ])],
        ),
        data_rows: 200,
    });

    // 6. 树面板：6 个筛选行 + 200 个树节点。
    fixtures.push(Fixture {
        name: "tree-panel",
        tree: card(
            "目录",
            vec![stack(vec![
                row(vec![text("目录"), spacer(), badge("200", "info")]),
                grid(2, {
                    (0..6)
                        .map(|index| row(vec![label(&format!("筛选 {index}")), input("值")]))
                        .collect()
                }),
                tree(200),
                divider(),
                row(vec![button("全部展开"), button("全部折叠")]),
            ])],
        ),
        data_rows: 200,
    });

    // 7. 深度 8 的嵌套布局（Stack/Row/Grid 混排，每层 4 个子节点）。
    fixtures.push(Fixture {
        name: "nested-layout",
        tree: nested(7),
        data_rows: 8 * 4,
    });

    // 8. 图表仪表盘：4 张卡（bars/line/donut/bars）+ 每卡 2 个图例文本。
    let chart_cards: Vec<Value> = ["bars", "line", "donut", "bars"]
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            card(
                &format!("图表 {index}"),
                vec![stack(vec![
                    text(&format!("指标 {index}")),
                    badge(kind, "info"),
                    chart(kind),
                    row(vec![label("图例 A"), label("图例 B")]),
                ])],
            )
        })
        .collect();
    fixtures.push(Fixture {
        name: "charts-dashboard",
        tree: grid(
            2,
            std::iter::once(row(vec![text("仪表盘"), spacer(), label("8 点")]))
                .chain(chart_cards)
                .collect(),
        ),
        data_rows: 4 * 8,
    });

    // 9. Markdown 面板：约 8 KB Markdown（单次提交的文本上限）+ 网格说明块。
    fixtures.push(Fixture {
        name: "markdown-8kb",
        tree: card(
            "说明",
            vec![stack(vec![
                row(vec![text("说明"), spacer(), badge("8 KB", "info")]),
                markdown_8kb(),
                divider(),
                row(vec![button("复制"), button("关闭")]),
                grid(2, cells(6)),
            ])],
        ),
        data_rows: 8192 / 60,
    });

    // 10. 代码面板：400 行代码（渲染上限）+ 键值 + 网格说明块。
    fixtures.push(Fixture {
        name: "code-400",
        tree: card(
            "代码",
            vec![stack(vec![
                row(vec![text("代码"), spacer(), badge("rust", "info")]),
                code_block(400),
                divider(),
                row(vec![button("复制"), button("打开")]),
                key_value(&[("行数", "400"), ("字节", "约 8 KB")]),
                grid(2, cells(6)),
            ])],
        ),
        data_rows: 400,
    });

    fixtures
}

// ---------------------------------------------------------------------------
// 统计
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Stats {
    p50: f64,
    p95: f64,
    min: f64,
    max: f64,
}

fn percentile(sorted: &[f64], percent: f64) -> f64 {
    let rank = ((percent / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn stats(samples: &mut [f64]) -> Stats {
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    Stats {
        p50: percentile(samples, 50.0),
        p95: percentile(samples, 95.0),
        min: samples[0],
        max: samples[samples.len() - 1],
    }
}

/// 解析后树的实际深度（含 `nested_children`）。
fn node_depth(node: &ElementNode) -> usize {
    let mut deepest = 0;
    for child in &node.children {
        deepest = deepest.max(node_depth(child));
    }
    for children in node.nested_children.values() {
        for child in children {
            deepest = deepest.max(node_depth(child));
        }
    }
    deepest + 1
}

/// 树里最大的单段文本字节数（`props.value` / `props.source`），用于核对 8 KB 配额。
fn max_text_bytes(value: &Value, current: &mut usize) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(props)) = map.get("props") {
                for key in ["value", "source"] {
                    if let Some(Value::String(text)) = props.get(key) {
                        *current = (*current).max(text.len());
                    }
                }
            }
            for child in map.values() {
                max_text_bytes(child, current);
            }
        }
        Value::Array(items) => {
            for item in items {
                max_text_bytes(item, current);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 探针
// ---------------------------------------------------------------------------

/// 满配额树：200 节点、深度 8（spec §6.1/§10 的单次提交上限）。
fn max_quota_tree() -> Value {
    let mut children: Vec<Value> = Vec::new();
    let mut nodes = 1; // 根 Stack
    while nodes + 31 <= 200 {
        // `nested(6)` = 31 节点、深度 7；套在根下正好是深度 8。
        children.push(nested(6));
        nodes += 31;
    }
    while nodes < 200 {
        children.push(text("filler"));
        nodes += 1;
    }
    stack(children)
}

/// 构造一棵满配额树并测 view/total 的 p95；返回 (nodes, depth, view, total)。
fn measure_max_quota(theme: &Theme) -> (usize, usize, Stats, Stats) {
    let tree = max_quota_tree();
    let parsed = parse_tree(&tree);
    let nodes = parsed.node_count();
    let depth = node_depth(&parsed.root);

    let mut view_samples = Vec::with_capacity(ROUNDS);
    let mut total_samples = Vec::with_capacity(ROUNDS);
    for _ in 0..WARMUP {
        let fresh = parse_tree(&tree);
        black_box(adc_view(theme, &fresh.root));
        black_box(adc_view(theme, &parsed.root));
    }
    for _ in 0..ROUNDS {
        let started = Instant::now();
        black_box(adc_view(theme, &parsed.root));
        view_samples.push(started.elapsed().as_secs_f64() * 1000.0);

        let started = Instant::now();
        let fresh = parse_tree(&tree);
        black_box(adc_view(theme, &fresh.root));
        total_samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    (
        nodes,
        depth,
        stats(&mut view_samples),
        stats(&mut total_samples),
    )
}

#[test]
fn adc_construction_budget_max_quota_tree() {
    let theme = Theme::light();
    let (nodes, depth, view, total) = measure_max_quota(&theme);
    println!(
        "ADC 满配额树（{} 构建）：nodes={nodes} depth={depth} view_p50={:.3} view_p95={:.3} total_p50={:.3} total_p95={:.3}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        view.p50,
        view.p95,
        total.p50,
        total.p95
    );
    println!(
        "ADC-PERF-MAX-QUOTA {}",
        json!({
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "nodes": nodes,
            "depth": depth,
            "rounds": ROUNDS,
            "view_p50_ms": view.p50,
            "view_p95_ms": view.p95,
            "total_p50_ms": total.p50,
            "total_p95_ms": total.p95,
        })
    );

    assert_eq!(nodes, 200, "满配额树必须是 200 节点");
    assert!(depth <= 8, "满配额树深度 {depth} 超过 8");
    assert!(
        view.p95 < P95_LIMIT_MS,
        "满配额树 adc_view 构造 p95 {:.3} ms 超过上界 {P95_LIMIT_MS} ms",
        view.p95
    );
    assert!(
        total.p95 < P95_LIMIT_MS,
        "满配额树 parse+construct p95 {:.3} ms 超过上界 {P95_LIMIT_MS} ms",
        total.p95
    );
}

#[test]
fn adc_construction_budget_typical_trees() {
    let theme = Theme::light();
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), 10, "预算探针必须覆盖 10 棵典型树");

    println!(
        "ADC 渲染预算（几何构造阶段；{} 构建，{} 轮/树，丢弃 {} 轮预热）",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        ROUNDS,
        WARMUP
    );
    println!(
        "{:<18} {:>5} {:>6} {:>8} {:>4} {:>9} {:>9} {:>9} {:>9} {:>10} {:>10}",
        "tree",
        "nodes",
        "rows",
        "max_txt",
        "a11y",
        "parse_p50",
        "parse_p95",
        "view_p50",
        "view_p95",
        "total_p50",
        "total_p95"
    );

    let mut summaries = Vec::with_capacity(fixtures.len());
    let mut shape_violations: Vec<String> = Vec::new();
    let mut worst_total_p95 = 0.0_f64;

    for fixture in &fixtures {
        let parsed = parse_tree(&fixture.tree);
        let nodes = parsed.node_count();
        if !(30..=200).contains(&nodes) {
            shape_violations.push(format!(
                "{}: ADC 节点数 {nodes} 不在 30..=200（典型树要求）",
                fixture.name
            ));
        }
        let depth = node_depth(&parsed.root);
        if depth > 8 {
            shape_violations.push(format!("{}: 深度 {depth} 超过配额 8", fixture.name));
        }
        let mut max_bytes = 0usize;
        max_text_bytes(&fixture.tree, &mut max_bytes);
        if max_bytes > 8192 {
            shape_violations.push(format!(
                "{}: 单段文本 {max_bytes} 字节超过 8 KB",
                fixture.name
            ));
        }
        let a11y_missing = parsed.a11y_missing().len();

        let mut parse_samples = Vec::with_capacity(ROUNDS);
        let mut view_samples = Vec::with_capacity(ROUNDS);
        let mut total_samples = Vec::with_capacity(ROUNDS);

        for _ in 0..WARMUP {
            let tree = parse_tree(&fixture.tree);
            black_box(adc_view(&theme, &tree.root));
            black_box(adc_view(&theme, &parsed.root));
        }

        for _ in 0..ROUNDS {
            // parse：纯解析成本。
            let started = Instant::now();
            let fresh = parse_tree(&fixture.tree);
            parse_samples.push(started.elapsed().as_secs_f64() * 1000.0);

            // view：复用同一棵解析树，只测元素构造成本。
            let started = Instant::now();
            black_box(adc_view(&theme, &parsed.root));
            view_samples.push(started.elapsed().as_secs_f64() * 1000.0);

            // total：面板一次提交的真实路径（parse + construct）。
            let started = Instant::now();
            black_box(adc_view(&theme, &fresh.root));
            total_samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }

        let parse_stats = stats(&mut parse_samples);
        let view_stats = stats(&mut view_samples);
        let total_stats = stats(&mut total_samples);
        worst_total_p95 = worst_total_p95.max(total_stats.p95);

        println!(
            "{:<18} {:>5} {:>6} {:>8} {:>4} {:>9.3} {:>9.3} {:>9.3} {:>9.3} {:>10.3} {:>10.3}",
            fixture.name,
            nodes,
            fixture.data_rows,
            max_bytes,
            a11y_missing,
            parse_stats.p50,
            parse_stats.p95,
            view_stats.p50,
            view_stats.p95,
            total_stats.p50,
            total_stats.p95
        );

        assert!(
            view_stats.p95 < P95_LIMIT_MS,
            "{} 的 adc_view 构造 p95 {:.3} ms 超过上界 {P95_LIMIT_MS} ms",
            fixture.name,
            view_stats.p95
        );
        assert!(
            total_stats.p95 < P95_LIMIT_MS,
            "{} 的 parse+construct p95 {:.3} ms 超过上界 {P95_LIMIT_MS} ms",
            fixture.name,
            total_stats.p95
        );

        summaries.push(json!({
            "tree": fixture.name,
            "nodes": nodes,
            "data_rows": fixture.data_rows,
            "max_text_bytes": max_bytes,
            "a11y_missing": a11y_missing,
            "parse_p50_ms": parse_stats.p50,
            "parse_p95_ms": parse_stats.p95,
            "view_p50_ms": view_stats.p50,
            "view_p95_ms": view_stats.p95,
            "total_p50_ms": total_stats.p50,
            "total_p95_ms": total_stats.p95,
            "total_min_ms": total_stats.min,
            "total_max_ms": total_stats.max,
        }));
    }

    println!(
        "ADC-PERF-JSON {}",
        json!({
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "rounds": ROUNDS,
            "warmup": WARMUP,
            "p95_limit_ms": P95_LIMIT_MS,
            "worst_total_p95_ms": worst_total_p95,
            "trees": summaries,
        })
    );

    // 10 棵树的总体上界：任何一棵都不允许触碰 50 ms，且最差的一棵留足余量。
    assert!(
        worst_total_p95 < P95_LIMIT_MS,
        "最差树 p95 {worst_total_p95:.3} ms 超过上界 {P95_LIMIT_MS} ms"
    );
    assert!(
        shape_violations.is_empty(),
        "树形状不符合预算形状约束: {shape_violations:#?}"
    );
}
