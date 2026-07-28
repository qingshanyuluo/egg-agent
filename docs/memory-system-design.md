# egg 记忆系统设计：从执行轨迹到自我归纳的经验树

> 目标：让 egg 在对话/执行过程中，由小模型在后台持续归档记忆，
> 像人类一样把"经历过的事"逐步抽象为"经验与技能"，自底向上长成一棵树，
> 并在后续任务中按需（progressive disclosure）加载。

---

## 一、业界方案调研

### 1.1 事实记忆派（"记住用户说过什么"）

| 方案 | 机制 | 局限 |
|---|---|---|
| **Mem0 / Mem0g** ([paper](https://arxiv.org/abs/2504.19413)) | 两阶段 pipeline：Extraction（从消息对抽取候选事实）→ Update（向量检索相似记忆，LLM 决策 ADD/UPDATE/DELETE/NOOP）。Mem0g 加实体-关系图。对话后异步处理 | 以"事实"为中心，不记"经验"；记忆是扁平条目列表，无层级抽象 |
| **MemGPT / Letta** | OS 分页隐喻：core memory（常驻）+ archival memory（外存），agent 自主调用 memory 工具编辑 | 靠主模型自我管理，占用主循环注意力和 token，贵 |
| **Zep** | 时序知识图谱（temporal KG），事实带有效期 | 工程重，仍是事实记忆 |
| **MemoryBank** | 艾宾浩斯遗忘曲线 + 每日总结 → 长期人格洞察 | 面向陪伴对话，不面向任务执行 |

### 1.2 经验学习派（"记住事是怎么做成的"）

| 方案 | 机制 | 局限 |
|---|---|---|
| **Reflexion** | 失败后口头反思，存入 episodic buffer，下轮重试 | 只作用于当前任务，无跨任务沉淀 |
| **ExpeL** ([AAAI 2024](https://ojs.aaai.org/index.php/AAAI/article/view/29936)) | 经验池（成功/失败轨迹）→ 跨任务 insight 抽取，用 **ADD / EDIT / UPVOTE / DOWNVOTE** 操作维护 insight 列表（带计数自净）；推理时召回 insights + 相似成功轨迹做 few-shot | 离线批量训练式，非对话中持续进行；insight 列表扁平 |
| **Voyager** ([NeurIPS 2023](https://arxiv.org/abs/2305.16291)) | 自动课程驱动探索 → 生成代码技能 → 环境反馈 + **自我验证**后入技能库，embedding 索引检索；技能可组合 | 绑定 Minecraft 环境；技能库扁平（向量表），无层级组织 |

### 1.3 组织/检索结构派

| 方案 | 机制 | 局限 |
|---|---|---|
| **A-MEM** ([NeurIPS 2025](https://arxiv.org/abs/2502.12110)) | Zettelkasten 卡片盒：每条记忆生成结构化 note（关键词、tags、上下文描述），自动与历史 note 建立链接，且新记忆触发旧记忆**演化**更新 | 扁平网络，无层级；每条新记忆都要 LLM 判链，成本高 |
| **RAPTOR** ([ICLR 2024](https://arxiv.org/abs/2401.18059)) | 递归 embedding → 软聚类 → LLM 摘要，自底向上长成**摘要树**；查询支持 tree traversal / collapsed tree | 是静态文档的检索索引，不是 agent 的能力地图；树建成后不演化 |
| **Generative Agents** | retrieval = recency + relevance + importance；importance 累积到阈值触发 reflection 生成高层记忆 | 提供了"触发式反思"的雏形，但面向模拟人生 |
| **Claude Agent Skills** ([docs](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview)) | **渐进式披露**：L1 元数据常驻（~100 token/skill）→ L2 SKILL.md 触发时加载 → L3 资源/脚本按需 | **全靠人工手写**；skill 之间是孤岛，没有全局树状组织；agent 不会自己长出新 skill |

### 1.4 综述视角

《From Storage to Experience: A Survey on the Evolution of LLM Agent Memory》把记忆机制演进分为三阶段：
**Storage（轨迹保存）→ Reflection（轨迹精炼）→ Experience（轨迹抽象）**。
经验阶段的两个核心机制正是：**主动探索**（agent 主动收集经验）和**跨轨迹抽象**（从多条轨迹中提炼可迁移的行为经验）。

### 1.5 行业空白（本设计的出发点）

1. **事实 ≠ 经验**：主流记"用户喜欢什么"，很少记"这个坑上次是怎么爬出来的"。
2. **Skills 是手工货架**：需要人写 SKILL.md，skill 之间无组织，agent 不会自我归纳。
3. **没有系统打通闭环**：执行轨迹 → 自动抽象 → 层级经验树 → 按需渐进加载 → 反哺执行。

---

## 二、设计：三层记忆 + 后台固化循环

### 2.1 认知映射（类比人类记忆）

| 人类 | egg | 载体 |
|---|---|---|
| 工作记忆 | 当前对话 history | 上下文窗口 |
| 情景记忆 | **Episode**（执行片段卡） | `memory/episodes/` |
| 语义/程序性记忆 | **Insight / Skill**（经验卡） | `memory/insights/`、`skills/` |
| 元认知（"我知道我会什么"） | **技能树**（多层摘要目录） | `memory/tree/` |
| 睡眠固化 | **Consolidation 循环**（小模型后台跑） | aux LLM |

### 2.2 总体架构

```
        主模型（对话/执行）
              │ AgentEvent::Done（异步，不阻塞）
              ▼
   ┌─────────────────────┐   每轮结束，1 次小模型调用
   │ L0 Episode 抽取      │   ── 显著性打分，琐事不记
   │ "发生了什么/坑/结果"  │
   └─────────┬───────────┘
             │ 攒够 N 条 / 会话空闲 / 手动 /consolidate
             ▼
   ┌─────────────────────┐   ExpeL 式跨轨迹抽象
   │ L1 Insight 归纳      │   ADD/EDIT/UPVOTE/DOWNVOTE
   │ "可复用的套路与教训"  │   计数自净，≥k 次验证晋升 Skill
   └─────────┬───────────┘
             │ 新叶插入 / 定期重整
             ▼
   ┌─────────────────────┐   RAPTOR 式自底向上
   │ L2 技能树            │   聚类→摘要→分裂→衰减
   │ "我会什么"的能力地图  │   树根常驻 system prompt
   └─────────────────────┘
             ▲
             │ 主模型按需下钻（memory_search / skill_tree 工具）
        按需加载回主对话
```

### 2.3 L0：Episode（情景记忆）

**触发**：`Plugin::on_agent_event` 收到 `AgentEvent::Done(history)` 时，异步 spawn 一次 aux 调用（egg 已有 aux 小模型通道，config 注释里本就预留了 "memory consolidation"）。

**aux prompt 要求输出**（JSON）：

```json
{
  "salience": 0-10,           // 显著性：有坑/有新知/有用户纠正才记；琐碎任务直接丢弃
  "task": "一句话任务",
  "domain_tags": ["rust", "cargo", "ratatui"],
  "outcome": "success|failure|partial",
  "pitfalls": ["ratatui 的 unstable-rendered-line-info 需要 pin minor 版本"],
  "procedure": "关键步骤摘要（<=5 步）",
  "verified_by": "cargo test 通过 / 用户确认 / 未验证"
}
```

落盘为 `~/.egg-agent/memory/episodes/2026-07/<id>.md`（frontmatter + 正文），
**不进入主对话上下文**——写入通道对主模型完全透明、零成本。

### 2.4 L1：Insight → Skill（经验/程序性记忆）

**触发**（任一）：累积 N 条未固化 episode；会话空闲；用户 `/consolidate`。

**固化循环**（aux 批量读最近 episode + 现有 insight 列表，执行 ExpeL 操作）：

- `ADD`：新教训/套路（初始计数 2）
- `EDIT`：修正表述或适用范围
- `UPVOTE`：又一episode佐证（计数+1）
- `DOWNVOTE`：反例（计数-1，归零删除）——让 noisy 小模型的错误结论**自动被冲刷**，这是用小模型的关键安全阀

**晋升机制**：一条 procedure 类 insight 计数 ≥ k（如 5）且跨 ≥ 2 个项目/会话出现
→ 由 aux（或主模型在空闲时）**自动生成 SKILL.md** 写入 `~/.egg-agent/skills/<name>/`，
格式与 Claude Skills 兼容（name/description frontmatter + 步骤 + 坑清单 + 可选脚本）。
**这就是"自我归纳落地 skill"，全程无需人写。**

### 2.5 L2：技能树（全局能力地图）

回答"没有把整个电脑所有 skill 总结为一棵树"的痛点：

- **叶子** = skill / insight 卡片
- **内部节点** = aux 对该分支的摘要（"Rust 构建问题" → "cargo 依赖冲突" / "feature 开关" …）
- **生长**：新叶按其 description 的 embedding（无 embedding 时退化用 domain_tags 重叠度）找到归属分支插入
- **重整**：分支 > m 个叶子时聚类分裂；长期未命中的分支按遗忘曲线降权、归档（MemoryBank 式；**检索命中则强化**）
- **树根** `tree/ROOT.md` + 每层 `index.md` 体积极小（每层 < 500 token），
  **仅树根常驻 system prompt** —— 相当于 agent 的元认知："我知道自己会什么、去哪找"

### 2.6 按需加载（对齐 Skills 的 progressive disclosure）

| 层级 | 何时进上下文 | 成本 |
|---|---|---|
| 树根 + 全部 skill 的 name/description | 常驻 system prompt | <1k token |
| 某分支 index / 某 skill 的 SKILL.md | 主模型判断相关，调工具下钻 | 触发时一次 |
| episode 原文、脚本、参考资料 | 工具返回，用完即弃 | 按需 |

给主模型新增 3 个工具（挂进 `ToolRegistry`）：

1. `skill_tree(path?)` — 浏览/下钻技能树（读 ROOT.md / index.md / SKILL.md）
2. `memory_search(query, scope?)` — 检索 episode/insight（v1 用 tag+关键词；v2 加 embedding 向量）
3. `memory_record(note)` — 主模型**主动**写入（保留 MemGPT 式自我导向通道，双通道写入）

### 2.7 与 egg 现有架构的映射

| 需求 | 现有机制 | 改动 |
|---|---|---|
| 小模型 | `config.aux` + `OpenAiClient::with_params` | 零改动，直接复用 |
| 对话中持续触发 | `Plugin::on_agent_event(&AgentEvent::Done)` | 新增 `MemoryPlugin` |
| 记忆工具 | `ToolRegistry` | 新增 `src/tools/memory.rs` |
| 存储 | `~/.egg-agent/`（sessions 已在用） | 新增 `memory/`、`skills/` 子目录，全本地纯文本 |
| 开关/命令 | Plugin `commands()` | `/memory`、`/consolidate`、`/skills` |

### 2.8 与业界的差异化

1. **mem0 记事实，egg 记经验**：episode → insight → skill 是抽象阶梯，逐级提炼
2. **Skills 是手工货架，egg 自动生长**：执行 → 自我发现（显著性捕捉）→ 自我归纳（固化循环）→ 自动入树
3. **树不是 RAG 索引，是能力地图**：agent 对"自己会什么"有元认知，能主动下钻、主动补齐短板
4. **双通道读写**：被动固化（后台小模型）+ 主动记取（主模型工具），像人既有无意识记忆也有刻意笔记

---

## 三、落地路线

| 阶段 | 内容 | 依赖 |
|---|---|---|
| **P0** | `MemoryPlugin`：`Done` 时 aux 抽取 episode（含 salience 门槛），写 `memory/episodes/`；`/memory` 查看 | 仅现有 aux 通道 |
| **P1** | 固化循环 + insight 列表（ExpeL 四操作 + 计数）；top insights 注入 system prompt；`memory_search`（tag/关键词版） | P0 |
| **P2** | insight 晋升自动写 SKILL.md；egg 启动时扫描 `skills/` 把 name/description 注入 prompt；`skill_tree` 工具 | P1 |
| **P3** | 技能树：聚类分裂 + 多层摘要 + 树重整（遗忘/强化） | embedding API 配置（可选，降级 tag 聚类） |
| **P4** | 评测：构造重复任务集，对比有/无记忆的成功率与步数；遗忘曲线调参 | P3 |

## 四、关键风险与对策

- **小模型抽象质量差** → UPVOTE/DOWNVOTE 计数自净；insight 必须引用证据 episode id（可追溯、可审计）
- **成本失控** → 每轮 Done 仅 1 次小调用（<500 token）；固化循环批量；显著性门槛挡住琐碎任务
- **记忆污染主对话** → 写入通道对主模型透明；加载通道只有 3 个工具 + 常驻 <1k token
- **递归摘要幻觉累积**（RAPTOR 已知 ~4%）→ 内部节点摘要强制附子节点 id 列表，支持下钻核对原文
