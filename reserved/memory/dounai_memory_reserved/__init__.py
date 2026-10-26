"""Reserved 记忆扩展插件 —— 契约桩实现（默认禁用）。

硬约束：

- 默认禁用（``disabled``）；不可卸载、不可删除；
- **禁止实现真实记忆管线功能**——这里只做契约层面的桩，
  三组接口（查询 / 健康探测 / 写回）按冻结 schema 返回结构化数据；
- 验收走 mock，因此本模块可独立启动、独立测试，不依赖任何真实存储。

为什么仍然要存在：契约一旦被删除，下游就无法按未来版本对接；
保留桩实现是为了让契约**可被机器校验**，而不是为了现在提供记忆能力。
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import Any, Literal

CONTRACT_VERSION = "1.0"

# 默认禁用：只有显式设置 DOUNAI_MEMORY_ENABLED=1 才认为启用。
# 注意：即便"启用"，本模块也只会返回空结果，不会真的读写记忆。
ENABLED_ENV = "DOUNAI_MEMORY_ENABLED"


def is_enabled() -> bool:
    """是否被显式启用。默认 False。"""
    return os.environ.get(ENABLED_ENV, "0").strip() in {"1", "true", "yes", "on"}


@dataclass(frozen=True)
class MemoryItem:
    """单条记忆（冻结 schema）。"""

    content: str
    timestamp: str
    importance: float
    score: float
    ref: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "content": self.content,
            "timestamp": self.timestamp,
            "importance": self.importance,
            "score": self.score,
            "ref": self.ref,
        }


@dataclass(frozen=True)
class QueryRequest:
    """查询请求（冻结 schema）。"""

    keywords: list[str] = field(default_factory=list)
    entities: list[str] = field(default_factory=list)
    time_range: dict[str, str] | None = None
    top_k: int = 5
    priority: Literal["high", "normal"] = "normal"

    @staticmethod
    def from_dict(raw: dict[str, Any]) -> QueryRequest:
        if not isinstance(raw, dict):
            raise ValueError("query 必须是对象")

        def str_list(key: str) -> list[str]:
            value = raw.get(key, [])
            if not isinstance(value, list):
                raise ValueError(f"{key} 必须是数组")
            return [str(item) for item in value if isinstance(item, (str, int, float))]

        time_range = raw.get("time_range")
        if time_range is not None and not isinstance(time_range, dict):
            raise ValueError("time_range 必须是对象或 null")

        top_k_raw = raw.get("top_k", 5)
        try:
            top_k = int(top_k_raw)
        except (TypeError, ValueError) as exc:
            raise ValueError("top_k 必须是整数") from exc
        if not 1 <= top_k <= 100:
            raise ValueError("top_k 必须在 1..100")

        priority = raw.get("priority", "normal")
        if priority not in {"high", "normal"}:
            raise ValueError("priority 只能是 high 或 normal")

        return QueryRequest(
            keywords=str_list("keywords"),
            entities=str_list("entities"),
            time_range=time_range,
            top_k=top_k,
            priority=priority,
        )


def query(request: dict[str, Any]) -> dict[str, Any]:
    """查询接口：始终返回空结果。

    「禁止实现真实记忆管线」是硬要求，因此这里不查任何存储，
    只保证响应结构完全符合冻结 schema。
    """
    QueryRequest.from_dict(request)
    return {"memories": [], "token_count": 0}


def health() -> dict[str, Any]:
    """健康探测：默认禁用时返回 ``unavailable``。"""
    if not is_enabled():
        return {
            "status": "unavailable",
            "version": CONTRACT_VERSION,
            "reason": "disabled",
        }
    return {"status": "available", "version": CONTRACT_VERSION}


def write_back(payload: dict[str, Any]) -> dict[str, Any]:
    """写回接口：接受合法载荷但不落盘，返回接受结果。"""
    if not isinstance(payload, dict):
        raise ValueError("payload 必须是对象")

    summary = payload.get("summary")
    if not isinstance(summary, str):
        raise ValueError("summary 必须是字符串")

    timestamp = payload.get("timestamp")
    if not isinstance(timestamp, str):
        raise ValueError("timestamp 必须是字符串")

    importance_raw = payload.get("importance", 0.0)
    try:
        importance = float(importance_raw)
    except (TypeError, ValueError) as exc:
        raise ValueError("importance 必须是数字") from exc
    if not 0.0 <= importance <= 1.0:
        raise ValueError("importance 必须在 0..1")

    embedding = payload.get("embedding")
    if embedding is not None and not isinstance(embedding, list):
        raise ValueError("embedding 必须是数组或 null")

    # 不落盘：仅回执。真实实现属于未来版本。
    return {
        "accepted": True,
        "stored": False,
        "reason": "reserved stub: pattern accepted but not persisted",
        "received_at": datetime.now(UTC).isoformat(),
    }
