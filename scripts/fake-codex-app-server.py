#!/usr/bin/env python3
import argparse
import base64
import hashlib
import json
import socket
import struct
import time


def read_exact(conn, length):
    data = b""
    while len(data) < length:
        chunk = conn.recv(length - len(data))
        if not chunk:
            raise EOFError("socket closed")
        data += chunk
    return data


def read_frame(conn):
    first, second = read_exact(conn, 2)
    opcode = first & 0x0F
    length = second & 0x7F
    masked = second & 0x80
    if length == 126:
        length = struct.unpack("!H", read_exact(conn, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", read_exact(conn, 8))[0]
    mask = read_exact(conn, 4) if masked else b""
    payload = read_exact(conn, length)
    if masked:
        payload = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    if opcode == 8:
        raise EOFError("websocket closed")
    return payload.decode("utf-8")


def send_text(conn, value):
    payload = json.dumps(value).encode("utf-8")
    header = bytes([0x81])
    if len(payload) < 126:
        header += bytes([len(payload)])
    elif len(payload) <= 0xFFFF:
        header += bytes([126]) + struct.pack("!H", len(payload))
    else:
        header += bytes([127]) + struct.pack("!Q", len(payload))
    conn.sendall(header + payload)


def accept_websocket(conn):
    request = b""
    while b"\r\n\r\n" not in request:
        request += conn.recv(4096)
    headers = request.decode("utf-8", "replace").split("\r\n")
    key = next(
        line.split(":", 1)[1].strip()
        for line in headers
        if line.lower().startswith("sec-websocket-key:")
    )
    accept = base64.b64encode(
        hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()
    ).decode()
    conn.sendall(
        (
            "HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n"
            "\r\n"
        ).encode()
    )


def serve(port_file):
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 0))
    server.listen(1)
    port = server.getsockname()[1]
    with open(port_file, "w", encoding="utf-8") as handle:
        handle.write(str(port))

    conn, _ = server.accept()
    with conn:
        accept_websocket(conn)
        while True:
            message = json.loads(read_frame(conn))
            method = message.get("method")
            request_id = message.get("id")
            if method == "initialize":
                send_text(conn, {"id": request_id, "result": {"serverInfo": {"name": "fake"}}})
            elif method == "thread/start":
                send_text(conn, {"id": request_id, "result": {"thread": {"id": "thread-smoke"}}})
            elif method == "thread/resume":
                send_text(conn, {"id": request_id, "result": {"thread": {"id": message["params"]["threadId"]}}})
            elif method == "turn/start":
                send_text(conn, {"id": request_id, "result": {"turn": {"id": "turn-smoke"}}})
                time.sleep(0.05)
                send_text(
                    conn,
                    {
                        "method": "item/agentMessage/delta",
                        "params": {"delta": "fake streamed response"},
                    },
                )
                send_text(
                    conn,
                    {
                        "method": "turn/completed",
                        "params": {"threadId": "thread-smoke", "turnId": "turn-smoke"},
                    },
                )
                return


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port-file", required=True)
    args = parser.parse_args()
    serve(args.port_file)


if __name__ == "__main__":
    main()
