# AionUi 后端 - 模块目录索引

## 概述

本文档是 AionUi 后端 API 规范的主索引。目标是从原始 TypeScript/Electron 代码库中梳理所有接口（REST API + IPC），描述其功能语义，为 Rust 重写提供指导。

**源项目**：`../AionUi-Bak`（Electron + TypeScript）
**目标项目**：`aionui-backend`（Rust，Cargo Workspace）

## 梳理方法

- **源码驱动**：逐模块从源码中提取接口定义
- **粒度**：功能语义级（描述"做什么"，而非"怎么实现"）
- **协议映射**：每个 IPC 接口标注目标协议（HTTP / WebSocket / HTTP+WebSocket）
- **公共类型**：梳理过程中标记候选公共类型，所有模块完成后集中提炼到 `01-common-types.md`
- **跨会话支持**：本索引追踪进度，新会话读取本文件即可恢复

## 文档模板

每个模块文档采用统一结构：

1. **概述** - 一句话描述模块职责
2. **REST API** - 端点、方法、请求参数、响应格式、功能语义、错误场景
3. **IPC 接口** - 通道名、目标协议、参数、功能语义、依赖模块
4. **数据模型** - 涉及的核心数据结构
5. **模块依赖** - 依赖谁 / 被谁依赖
6. **候选公共类型** - 可能归入公共 crate 的类型

## 模块列表

| # | 模块 | 文档 | 源码位置 | 状态 |
|---|------|------|---------|------|
| 1 | 公共类型与 Trait | 01-common-types.md | （从所有模块提炼） | ⬜ 待提炼（所有模块完成后） |
| 2 | 数据模型与存储 | 02-database.md | `src/process/services/database/` | ⬜ 未开始 |
| 3 | 认证与用户管理 | 03-auth.md | `src/process/webserver/auth/`, `src/process/bridge/authBridge.ts` | ⬜ 未开始 |
| 4 | 系统设置 | 04-system-settings.md | `src/process/bridge/systemSettingsBridge.ts` | ⬜ 未开始 |
| 5 | 会话与消息管理 | 05-conversation.md | `src/process/bridge/conversationBridge.ts`, `src/process/task/` | ⬜ 未开始 |
| 6 | AI 后端集成 | 06-ai-agent.md | `src/process/agent/`, `src/process/task/*AgentManager.ts` | ⬜ 未开始 |
| 7 | 实时通信（WebSocket） | 07-realtime.md | `src/process/webserver/websocket/` | ⬜ 未开始 |
| 8 | 文件与工作区 | 08-file-workspace.md | `src/process/bridge/fsBridge.ts`, `src/process/bridge/documentBridge.ts` | ⬜ 未开始 |
| 9 | 通道集成 | 09-channel.md | `src/process/channels/` | ⬜ 未开始 |
| 10 | 团队模式 | 10-team.md | `src/process/team/` | ⬜ 未开始 |
| 11 | 定时任务 | 11-cron.md | `src/process/services/cron/` | ⬜ 未开始 |
| 12 | MCP 协议 | 12-mcp.md | `src/process/services/mcpServices/` | ⬜ 未开始 |
| 13 | 扩展系统 | 13-extension.md | `src/process/extensions/` | ⬜ 未开始 |
| 14 | 应用生命周期 | 14-app-lifecycle.md | `src/process/bridge/updateBridge.ts`, `src/process/bridge/applicationBridge.ts` | ⬜ 未开始 |
| 99 | Rust Crate 映射 | 99-rust-crate-mapping.md | （从所有模块推导） | ⬜ 待推导（所有模块完成后） |

## 梳理顺序

按依赖拓扑排序，基础模块优先：

```
数据库 (2) → 认证 (3) → 系统设置 (4)
    → 会话 (5) → AI 后端 (6) → 实时通信 (7)
    → 文件与工作区 (8) → 通道 (9) → 团队 (10)
    → 定时任务 (11) → MCP (12) → 扩展 (13) → 应用生命周期 (14)
    → 公共类型 (1) → Crate 映射 (99)
```

## Rust Workspace 结构（初步）

```
aionui-backend/
├── Cargo.toml                    # workspace 根配置
├── crates/
│   ├── aionui-common/            # 公共类型、错误定义、工具函数
│   ├── aionui-db/                # 数据库层（SQLite、迁移、Repository trait）
│   ├── aionui-api-types/         # HTTP/WS 请求响应 DTO
│   ├── aionui-auth/              # 认证与用户管理
│   ├── aionui-conversation/      # 会话与消息管理
│   ├── aionui-ai-agent/          # AI 后端集成
│   ├── aionui-realtime/          # WebSocket 实时通信
│   ├── aionui-file/              # 文件与工作区管理
│   ├── aionui-channel/           # 通道集成（Telegram、Slack 等）
│   ├── aionui-team/              # 团队模式
│   ├── aionui-cron/              # 定时任务
│   ├── aionui-mcp/               # MCP 协议
│   ├── aionui-extension/         # 扩展系统
│   ├── aionui-system/            # 系统设置 + 应用生命周期
│   └── aionui-app/               # 顶层组装：路由、启动入口
```

> 此结构为初步规划，最终映射将在 `99-rust-crate-mapping.md` 中确定。

## Crate 间通信原则

- Crate 之间通过 trait 通信，不直接依赖具体实现
- 依赖方向严格向下，禁止循环依赖
- `aionui-app` 是唯一知道所有 crate 的地方，负责依赖注入和组装
- `aionui-common` 是最底层，零业务逻辑
