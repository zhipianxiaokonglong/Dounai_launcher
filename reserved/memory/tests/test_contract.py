"""Reserved 记忆插件的契约测试（mock 验收）。

验证点：
1. 默认必须处于禁用状态；
2. 三组接口（查询 / 健康 / 写回）结构完全符合冻结 schema；
3. 不实现真实记忆——查询恒为空、写回不落盘；
4. 非法载荷返回 422 而不是崩溃。
"""

from __future__ import annotations

import os

import pytest
from fastapi.testclient import TestClient

from dounai_memory_reserved import CONTRACT_VERSION, health, query, is_enabled, write_back
from dounai_memory_reserved.app import app


@pytest.fixture(autouse=True)
def _clear_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("DOUNAI_MEMORY_ENABLED", raising=False)


@pytest.fixture
def client() -> TestClient:
    return TestClient(app)


class TestDefaultDisabled:
    def test_disabled_by_default(self) -> None:
        assert is_enabled() is False

    def test_health_unavailable_when_disabled(self) -> None:
        assert health() == {
            "status": "unavailable",
            "version": CONTRACT_VERSION,
            "reason": "disabled",
        }

    def test_enabled_flag_is_honoured(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("DOUNAI_MEMORY_ENABLED", "1")
        assert is_enabled() is True
        assert health()["status"] == "available"

    @pytest.mark.parametrize("value", ["0", "", "false", "no", "off"])
    def test_falsey_values_keep_it_disabled(self, monkeypatch: pytest.MonkeyPatch, value: str) -> None:
        monkeypatch.setenv("DOUNAI_MEMORY_ENABLED", value)
        assert is_enabled() is False


class TestQueryContract:
    def test_returns_empty_result_shape(self) -> None:
        result = query({"keywords": ["咖啡"], "entities": [], "top_k": 5, "priority": "normal"})
        assert result == {"memories": [], "token_count": 0}

    def test_accepts_full_schema_payload(self) -> None:
        result = query(
            {
                "keywords": ["a"],
                "entities": ["b"],
                "time_range": {"start": "2026-01-01T00:00:00Z", "end": "2026-12-31T00:00:00Z"},
                "top_k": 3,
                "priority": "high",
            }
        )
        assert result["memories"] == []

    def test_rejects_bad_top_k(self) -> None:
        with pytest.raises(ValueError):
            query({"top_k": 0})
        with pytest.raises(ValueError):
            query({"top_k": 101})

    def test_rejects_bad_priority(self) -> None:
        with pytest.raises(ValueError):
            query({"priority": "urgent"})

    def test_rejects_non_array_keywords(self) -> None:
        with pytest.raises(ValueError):
            query({"keywords": "not-a-list"})

    def test_http_endpoint_shape(self, client: TestClient) -> None:
        resp = client.post("/v1/memory/query", json={"keywords": ["x"], "top_k": 5})
        assert resp.status_code == 200
        assert resp.json() == {"memories": [], "token_count": 0}

    def test_http_rejects_invalid(self, client: TestClient) -> None:
        resp = client.post("/v1/memory/query", json={"top_k": 0})
        assert resp.status_code == 422


class TestWriteBackContract:
    def test_accepts_and_does_not_persist(self) -> None:
        result = write_back(
            {
                "timestamp": "2026-10-26T00:00:00Z",
                "summary": "用户提到最近睡得晚",
                "embedding": None,
                "importance": 0.6,
            }
        )
        assert result["accepted"] is True
        # 关键：不落盘，证明没有实现真实记忆管线
        assert result["stored"] is False
        assert "reserved" in result["reason"]

    @pytest.mark.parametrize(
        "payload",
        [
            {"summary": "x"},  # 缺 timestamp
            {"timestamp": "t"},  # 缺 summary
            {"timestamp": "t", "summary": "s", "importance": 1.5},  # 越界
            {"timestamp": "t", "summary": "s", "embedding": "not-a-list"},
        ],
    )
    def test_rejects_invalid_payload(self, payload: dict) -> None:
        with pytest.raises(ValueError):
            write_back(payload)

    def test_http_endpoint(self, client: TestClient) -> None:
        resp = client.post(
            "/v1/memory/write",
            json={"timestamp": "2026-10-26T00:00:00Z", "summary": "s", "importance": 0.2},
        )
        assert resp.status_code == 200
        assert resp.json()["stored"] is False

    def test_http_rejects_invalid(self, client: TestClient) -> None:
        resp = client.post("/v1/memory/write", json={"timestamp": "t"})
        assert resp.status_code == 422


class TestStatusEndpoint:
    def test_reports_not_removable(self, client: TestClient) -> None:
        resp = client.get("/v1/memory/status")
        body = resp.json()
        assert resp.status_code == 200
        assert body["removable"] is False
        assert body["enabled"] is False
        assert body["contract_version"] == CONTRACT_VERSION

    def test_health_endpoint(self, client: TestClient) -> None:
        resp = client.get("/health")
        assert resp.json()["status"] == "unavailable"
