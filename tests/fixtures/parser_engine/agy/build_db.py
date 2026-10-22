#!/usr/bin/env python3
"""Synthetic agy trajectory fixtures.

The parser crate never opens SQLite. This script writes the JSONL stream the
adapter consumes and, for the pipeline test, a SQLite database of the measured
Antigravity schema. Payloads are hand-encoded protobuf. No private session
bytes are copied.
"""

from __future__ import annotations

import argparse
import base64
import datetime
import json
import sqlite3
from pathlib import Path

NEEDLE = "Preserve the agy request about lexical search."
FIRST_TOKEN = "agypipelineinitial91dbea"
IDE_TOKEN = "agypipelineideroot47fdae"
TOOL_TOKEN = "agytoolonly893fed"
DISPATCH = "AGY_DISPATCH_NOT_INTENT"
BOOKKEEPING = "AGY_BOOKKEEPING_FIELD30_DO_NOT_PROJECT"
UNKNOWN = "AGY_UNKNOWN_STEP_TEXT_DO_NOT_PROJECT"
BAD_FORMAT = "AGY_BAD_FORMAT_DO_NOT_PROJECT"
CLI_ID = "11111111-1111-4111-8111-111111111111"
IDE_ID = "22222222-2222-4222-8222-222222222222"
CLI_TRAJECTORY = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
IDE_TRAJECTORY = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
CANONICAL_CWD = "/tmp/agy-fixture/work"
STAMP = int(
    datetime.datetime(2026, 9, 29, 12, 0, tzinfo=datetime.timezone.utc).timestamp()
)


def varint(number: int) -> bytes:
    out = bytearray()
    while True:
        byte = number & 0x7F
        number >>= 7
        if number:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def key(field: int, wire: int) -> bytes:
    return varint((field << 3) | wire)


def ld(field: int, payload: bytes) -> bytes:
    return key(field, 2) + varint(len(payload)) + payload


def vi(field: int, value: int) -> bytes:
    return key(field, 0) + varint(value)


def text(field: int, value: str) -> bytes:
    return ld(field, value.encode())


def msg(field: int, *parts: bytes) -> bytes:
    return ld(field, b"".join(parts))


def b64(payload: bytes) -> str:
    return base64.b64encode(payload).decode("ascii")


def header(cascade_id: str, trajectory_id: str, cwd: str) -> bytes:
    return b"".join(
        [
            msg(2, vi(1, STAMP), vi(2, 0)),
            text(3, trajectory_id),
            text(6, cascade_id),
            text(7, f"file://{cwd}"),
            text(18, "agy-fixture"),
        ]
    )


def meta(step_type: int, step_uuid: str, *inside_five: bytes) -> bytes:
    return b"".join(
        [
            vi(1, step_type),
            msg(
                5,
                msg(1, vi(1, STAMP), vi(2, 0)),
                text(12, step_uuid),
                *inside_five,
            ),
        ]
    )


def user_step(step_uuid: str, body: str) -> bytes:
    return meta(14, step_uuid) + msg(19, msg(3, text(1, body)))


def planner_step(step_uuid: str, field: int | None, body: str) -> bytes:
    payload = meta(15, step_uuid)
    if field is None:
        return payload
    return payload + msg(20, text(field, body), text(6, "planner-signature"))


def tool_step(step_uuid: str, cwd: str, command: str, result: str) -> bytes:
    arguments = json.dumps({"command": command, "Cwd": cwd}, separators=(",", ":"))
    cwd_field = json.dumps({"Cwd": cwd}, separators=(",", ":"))
    opaque = b"opaque-tool-bytes\xff"
    return meta(
        132,
        step_uuid,
        msg(
            4,
            text(1, "call-agy-fixture"),
            text(2, "run_command"),
            text(3, arguments),
            msg(7, msg(2, ld(1, opaque))),
        ),
    ) + msg(
        140,
        text(1, cwd_field),
        # Live field 140.2 carries the rendered text at 140.2.1 beside a
        # non-UTF-8 sibling. Without that sibling the container is ASCII and
        # the walker would keep it as text.
        msg(2, text(1, result), ld(6, b"\xff")),
    )


def bookkeeping_step() -> bytes:
    return meta(23, "step-bookkeeping") + msg(30, text(5, BOOKKEEPING))


def unknown_step() -> bytes:
    return meta(8, "step-unknown") + text(24, UNKNOWN)


def bad_format_step() -> bytes:
    return user_step("step-bad-format", BAD_FORMAT)


def opaque_tool_payload(secret: str) -> bytes:
    return meta(
        132,
        "step-opaque",
        msg(
            4,
            text(1, "opaque-call"),
            text(2, "view_file"),
            text(3, "{}"),
            msg(7, msg(2, ld(1, secret.encode() + b"\xff"))),
        ),
    )


def step_line(
    idx: int,
    step_type: int,
    payload: bytes,
    *,
    status: int | None = 3,
    step_format: int | None = 0,
    note: str | None = None,
) -> str:
    row: dict[str, object] = {}
    if note is not None:
        row["note"] = note
    row.update(
        {
            "idx": idx,
            "payload_b64": b64(payload),
            "record": "step",
            "status": status,
            "step_format": step_format,
            "step_type": step_type,
            "v": 1,
        }
    )
    return json.dumps(row, separators=(",", ":"))


def header_line(metadata: bytes) -> str:
    row = {"metadata_b64": b64(metadata), "record": "header", "v": 1}
    return json.dumps(row, separators=(",", ":"))


def cli_lines(cwd: str) -> list[str]:
    human = (
        f"{NEEDLE}\n"
        f"Decision: keep lexical search for {FIRST_TOKEN}.\n"
        "Should lexical search stay?"
    )
    planner = f"Decision: keep lexical search for {FIRST_TOKEN}."
    lines = [
        header_line(header(CLI_ID, CLI_TRAJECTORY, cwd)),
        step_line(
            1,
            14,
            user_step(
                "step-dispatch",
                "You are running under Vibecrafted core runtime.\n" + DISPATCH,
            ),
        ),
        step_line(2, 14, user_step("step-human", human), note=NEEDLE),
        step_line(3, 15, planner_step("step-planner-20-3", 3, planner)),
        step_line(
            4,
            15,
            planner_step("step-planner-20-1", 1, "The 20.1 planner line stays visible."),
        ),
        step_line(5, 15, planner_step("step-planner-empty", None, "")),
        step_line(
            6,
            132,
            tool_step(
                "step-tool",
                cwd,
                "cargo test -p aicx-parser",
                f"test result: ok {TOOL_TOKEN}",
            ),
        ),
        step_line(7, 23, bookkeeping_step()),
        step_line(8, 8, unknown_step()),
        step_line(9, 14, bad_format_step(), step_format=1),
        step_line(10, 14, b"\x80"),
        step_line(
            11,
            15,
            planner_step("step-null-status", 3, "Null status still speaks."),
            status=None,
        ),
        step_line(12, 101, b""),
    ]
    return lines


def ide_lines(cwd: str) -> list[str]:
    human = f"Decision: keep the IDE root for {IDE_TOKEN}."
    return [
        header_line(header(IDE_ID, IDE_TRAJECTORY, cwd)),
        step_line(1, 14, user_step("step-ide-human", human)),
        step_line(
            2,
            15,
            planner_step("step-ide-planner", 3, f"The IDE root keeps {IDE_TOKEN}."),
        ),
    ]


def parse_line(line: str) -> tuple[str, dict]:
    row = json.loads(line)
    return row["record"], row


def write_db(path: Path, lines: list[str], cascade_id: str, trajectory_id: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        path.unlink()
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE trajectory_meta (
            trajectory_id TEXT,
            cascade_id TEXT,
            trajectory_type INTEGER,
            source INTEGER
        );
        CREATE TABLE steps (
            idx INTEGER PRIMARY KEY,
            step_type INTEGER,
            status INTEGER,
            has_subtrajectory INTEGER,
            metadata BLOB,
            error_details BLOB,
            permissions BLOB,
            task_details BLOB,
            render_info BLOB,
            step_payload BLOB,
            step_format INTEGER
        );
        CREATE TABLE trajectory_metadata_blob (
            id TEXT PRIMARY KEY,
            data BLOB
        );
        CREATE TABLE gen_metadata (id TEXT);
        CREATE TABLE executor_metadata (id TEXT);
        CREATE TABLE parent_references (id TEXT);
        CREATE TABLE battle_mode_infos (id TEXT);
        """
    )
    conn.execute(
        "INSERT INTO trajectory_meta (trajectory_id, cascade_id, trajectory_type, source) VALUES (?, ?, 4, 17)",
        (trajectory_id, cascade_id),
    )
    for line in lines:
        kind, row = parse_line(line)
        if kind == "header":
            conn.execute(
                "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?)",
                (base64.b64decode(row["metadata_b64"]),),
            )
            continue
        payload = base64.b64decode(row["payload_b64"]) if row["payload_b64"] else None
        conn.execute(
            """
            INSERT INTO steps (
                idx, step_type, status, has_subtrajectory, step_payload, step_format
            ) VALUES (?, ?, ?, 0, ?, ?)
            """,
            (
                row["idx"],
                row["step_type"],
                row["status"],
                payload,
                row["step_format"],
            ),
        )
    conn.commit()
    conn.close()


def write_text(path: Path, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(f"{line}\n" for line in lines), encoding="utf-8")


def emit(home: Path, cwd: str) -> None:
    cli_root = home / ".gemini" / "antigravity-cli" / "conversations"
    ide_root = home / ".gemini" / "antigravity" / "conversations"
    write_db(cli_root / f"{CLI_ID}.db", cli_lines(cwd), CLI_ID, CLI_TRAJECTORY)
    write_db(ide_root / f"{IDE_ID}.db", ide_lines(cwd), IDE_ID, IDE_TRAJECTORY)
    (cli_root / "legacy-export.pb").write_bytes(b"legacy-pb-not-a-session")
    (cli_root / "not-a-session.db").write_bytes(b"not-a-uuid-database")
    history = home / ".gemini" / "antigravity-cli" / "history.jsonl"
    history.write_text(
        json.dumps(
            {
                "conversationId": "not-a-database",
                "workspace": cwd,
                "timestamp": "2026-09-29T12:00:00Z",
                "type": "user",
                "text": "history jsonl is not a second source",
            }
        )
        + "\n",
        encoding="utf-8",
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    fixtures = sub.add_parser("write-fixtures")
    fixtures.add_argument("--out", type=Path, required=True)
    fixtures.add_argument("--cwd", default=CANONICAL_CWD)
    emitted = sub.add_parser("emit")
    emitted.add_argument("--home", type=Path, required=True)
    emitted.add_argument("--cwd", required=True)
    opaque = sub.add_parser("opaque-b64")
    opaque.add_argument("--secret", required=True)
    args = parser.parse_args()
    if args.cmd == "write-fixtures":
        write_text(args.out / "cli.jsonl", cli_lines(args.cwd))
        write_text(args.out / "ide.jsonl", ide_lines(args.cwd))
    elif args.cmd == "emit":
        emit(args.home, args.cwd)
    else:
        print(b64(opaque_tool_payload(args.secret)))


if __name__ == "__main__":
    main()
