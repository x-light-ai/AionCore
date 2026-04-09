# AionUi 后端重写 - 设计规范

**日期**：2026-04-09
**状态**：已批准

## 目标

将 AionUi 后端从 TypeScript/Electron 重写为 Rust，基于现有 API 接口（REST API + IPC）进行重写，而非源码重构。Rust 后端是独立的 HTTP/WebSocket 服务，同时服务 Electron 和浏览器客户端。

## 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 梳理方案 | 源码驱动接口提取 | 确保接口不遗漏 |
| 梳理粒度 | 功能语义级 | 描述"做什么"而非"怎么做"，避免照搬不良实现 |
| 架构 | Cargo Workspace + 多 crate | 强制模块解耦，清晰的 API 边界 |
| 通信协议 | HTTP + WebSocket | HTTP 用于请求-响应，WebSocket 用于流式/实时 |
| 公共类型 | 两轮提炼 | 梳理时标记候选，所有模块完成后集中提炼 |
| 前端 | 同时支持 Electron 和 Web | 后端协议无关，前端是薄客户端 |

## 接口梳理流程

### 第一步：模块目录索引
产出 `docs/api-spec/00-module-index.md`，列出所有模块、职责、源码位置和梳理状态。

### 第二步：逐模块分析（按依赖顺序）
每个模块从源码中提取：
- REST API 端点（方法、URL、参数、响应、功能语义、错误场景）
- IPC 接口及目标协议标注（HTTP / WebSocket / HTTP+WebSocket）
- 涉及的数据模型
- 模块依赖关系
- 候选公共类型

梳理顺序按依赖拓扑：
1. 数据库 → 认证 → 系统设置
2. 会话 → AI 后端 → 实时通信
3. 文件与工作区 → 通道 → 团队
4. 定时任务 → MCP → 扩展 → 应用生命周期

### 第三步：公共类型提炼
所有模块梳理完成后，审视所有候选公共类型，产出 `01-common-types.md`。

### 第四步：Rust Crate 映射
产出 `99-rust-crate-mapping.md`，确定最终 Workspace 结构、crate 职责和依赖图。

## 文档结构

```
docs/api-spec/
├── 00-module-index.md          # 主索引，含进度追踪
├── 01-common-types.md          # 公共类型（最后产出）
├── 02-database.md              # 数据模型与存储
├── 03-auth.md                  # 认证与用户管理
├── 04-system-settings.md       # 系统设置
├── 05-conversation.md          # 会话与消息管理
├── 06-ai-agent.md              # AI 后端集成
├── 07-realtime.md              # WebSocket 实时通信
├── 08-file-workspace.md        # 文件与工作区
├── 09-channel.md               # 通道集成
├── 10-team.md                  # 团队模式
├── 11-cron.md                  # 定时任务
├── 12-mcp.md                   # MCP 协议
├── 13-extension.md             # 扩展系统
├── 14-app-lifecycle.md         # 应用生命周期
└── 99-rust-crate-mapping.md    # Crate 映射（最后产出）
```

## 模块文档模板

每个模块文档结构：

1. **概述** - 一句话描述职责
2. **REST API** - 逐端点：方法、URL、参数表、响应表、功能语义、错误场景
3. **IPC 接口** - 逐通道：目标协议（HTTP/WebSocket/HTTP+WebSocket）、参数、功能语义、依赖模块
4. **数据模型** - 核心数据结构
5. **模块依赖** - 依赖谁 / 被谁依赖
6. **候选公共类型** - 可能归入 aionui-common 的类型

## Rust Workspace 结构（初步）

```
aionui-backend/
├── Cargo.toml
├── crates/
│   ├── aionui-common/            # 公共类型、错误定义、工具函数（零业务逻辑）
│   ├── aionui-db/                # 数据库层（SQLite、迁移、Repository trait）
│   ├── aionui-api-types/         # HTTP/WS 请求响应 DTO
│   ├── aionui-auth/              # 认证与用户管理
│   ├── aionui-conversation/      # 会话与消息管理
│   ├── aionui-ai-agent/          # AI 后端集成
│   ├── aionui-realtime/          # WebSocket 实时通信
│   ├── aionui-file/              # 文件与工作区管理
│   ├── aionui-channel/           # 通道集成
│   ├── aionui-team/              # 团队模式
│   ├── aionui-cron/              # 定时任务
│   ├── aionui-mcp/               # MCP 协议
│   ├── aionui-extension/         # 扩展系统
│   ├── aionui-system/            # 系统设置 + 应用生命周期
│   └── aionui-app/               # 顶层组装
```

### Crate 依赖方向

```
aionui-common（最底层，无依赖）
    ↑
aionui-db（依赖 common）
    ↑
aionui-api-types（依赖 common）
    ↑
各业务 crate（依赖 common + db + api-types）
    ↑
aionui-app（顶层，依赖所有 crate）
```

### 通信原则
- Crate 之间通过 trait 通信，不直接依赖具体实现
- 依赖方向严格向下，禁止循环依赖
- `aionui-app` 是唯一的组装者，负责依赖注入
- `aionui-common` 零业务逻辑

## 跨会话支持

`00-module-index.md` 的状态列追踪进度。新会话操作：
1. 读取 `00-module-index.md` 找到下一个待梳理模块
2. 读取 AionUi-Bak 对应模块的源码
3. 参考已完成的模块文档保持格式一致
4. 产出下一个模块文档
