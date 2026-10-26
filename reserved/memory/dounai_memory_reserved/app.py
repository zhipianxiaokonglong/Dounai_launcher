"""Reserved 记忆插件的最小 HTTP 服务（默认禁用状态）。

仅用于契约验收：让三组接口可以被真实 HTTP 调用打通，
以便下游/评审用 mock 走通调用链。

不做任何真实记忆读写。
"""

from __future__ import annotations

from typing import Any

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

from . import CONTRACT_VERSION, health, is_enabled, query, write_back

app = FastAPI(
    title="Dounai Reserved Memory (stub)",
    version=CONTRACT_VERSION,
    description="契约桩：默认禁用，不实现真实记忆管线。",
)


class TimeRange(BaseModel):
    start: str | None = None
    end: str | None = None


class QueryBody(BaseModel):
    keywords: list[str] = Field(default_factory=list)
    entities: list[str] = Field(default_factory=list)
    time_range: TimeRange | None = None
    top_k: int = 5
    priority: str = "normal"


class WriteBody(BaseModel):
    timestamp: str
    summary: str
    embedding: list[float] | None = None
    importance: float = 0.0


@app.get("/health")
def health_endpoint() -> dict[str, Any]:
    return health()


@app.post("/v1/memory/query")
def query_endpoint(body: QueryBody) -> dict[str, Any]:
    payload = body.model_dump()
    time_range = payload.get("time_range")
    if isinstance(time_range, dict):
        # 过滤掉 None，保持与冻结 schema 一致
        payload["time_range"] = {k: v for k, v in time_range.items() if v is not None}
    try:
        return query(payload)
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc


@app.post("/v1/memory/write")
def write_endpoint(body: WriteBody) -> dict[str, Any]:
    try:
        return write_back(body.model_dump())
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc


@app.get("/v1/memory/status")
def status_endpoint() -> dict[str, Any]:
    return {
        "enabled": is_enabled(),
        "status": health()["status"],
        "contract_version": CONTRACT_VERSION,
        "removable": False,
        "note": "Reserved 插件：默认禁用，契约冻结，禁止删除。",
    }
