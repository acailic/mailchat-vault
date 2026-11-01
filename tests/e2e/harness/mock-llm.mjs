// A mock of the Anthropic-compatible `/v1/messages` endpoint the LLM search
// talks to (see `vault_core::llm::AnthropicLlm`). It serves a scripted
// sequence of replies (first call = translation, second = summary, ...) and
// records every request so specs can assert exactly what left the machine.

import http from "node:http";

/**
 * @param {Array<string | { status?: number, body?: unknown }>} replies served
 *   in order; a plain string becomes a successful text reply, an object can
 *   describe an HTTP error. Past the end of the queue the server answers 500.
 * @returns {Promise<{ port: number, requests: Array<{ headers: Record<string,string>, body: any }>, close(): Promise<void> }>}
 */
export function startMockLlm(replies) {
  const requests = [];
  let next = 0;

  const server = http.createServer((req, res) => {
    if (req.method !== "POST" || !req.url.endsWith("/v1/messages")) {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: `unexpected ${req.method} ${req.url}` }));
      return;
    }
    let raw = "";
    req.on("data", (chunk) => {
      raw += chunk;
    });
    req.on("end", () => {
      let body = null;
      try {
        body = JSON.parse(raw);
      } catch {
        body = raw;
      }
      requests.push({ headers: req.headers, body });
      const reply = replies[next++];
      if (reply === undefined) {
        res.writeHead(500, { "content-type": "application/json" });
        res.end(
          JSON.stringify({ error: { type: "api_error", message: "mock reply queue exhausted" } }),
        );
        return;
      }
      if (typeof reply !== "string") {
        res.writeHead(reply.status ?? 500, { "content-type": "application/json" });
        res.end(JSON.stringify(reply.body ?? { error: { message: "mock error" } }));
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          id: `msg_mock_${next}`,
          content: [{ type: "text", text: reply }],
        }),
      );
    });
  });

  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      resolve({
        port: server.address().port,
        requests,
        close: () => new Promise((done) => server.close(() => done())),
      });
    });
  });
}
