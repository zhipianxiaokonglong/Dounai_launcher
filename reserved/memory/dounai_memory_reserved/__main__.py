"""命令行入口：python -m dounai_memory_reserved"""

from __future__ import annotations

import argparse
import os

from .app import app


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="dounai-memory-reserved")
    sub = parser.add_subparsers(dest="command", required=True)
    serve = sub.add_parser("serve", help="启动契约桩服务")
    serve.add_argument("--host", default="127.0.0.1")
    serve.add_argument("--port", type=int, default=int(os.environ.get("DOUNAI_MEMORY_PORT", 8104)))

    args = parser.parse_args(argv)
    if args.command == "serve":
        import uvicorn

        uvicorn.run(app, host=args.host, port=args.port, log_level="info")
        return 0
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
