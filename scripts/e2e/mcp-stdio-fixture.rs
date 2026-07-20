use std::io::{self, BufRead, Write};

fn request_id(line: &str) -> &str {
    line.split("\"id\":")
        .nth(1)
        .expect("request id")
        .split(|character| character == ',' || character == '}')
        .next()
        .expect("request id value")
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line.expect("JSON-RPC input");
        if line.contains("\"method\":\"initialize\"") {
            writeln!(
                stdout,
                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{{}}}}}}",
                request_id(&line)
            )
            .expect("initialize response");
        } else if line.contains("\"method\":\"tools/list\"") {
            writeln!(
                stdout,
                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"tools\":[{{\"name\":\"echo\",\"description\":\"Echo a value through the deterministic E2E fixture\",\"inputSchema\":{{\"type\":\"object\",\"properties\":{{\"text\":{{\"type\":\"string\"}}}},\"required\":[\"text\"],\"additionalProperties\":false}}}}]}}}}",
                request_id(&line)
            )
            .expect("tools list response");
        } else if line.contains("\"method\":\"tools/call\"") {
            writeln!(
                stdout,
                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"MCP_E2E_PRIVATE_RESULT_DO_NOT_EXPOSE\"}}],\"isError\":false}}}}",
                request_id(&line)
            )
            .expect("tools call response");
        }
        stdout.flush().expect("JSON-RPC flush");
    }
}
