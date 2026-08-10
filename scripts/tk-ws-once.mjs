// One-shot Threadknot WS client: sends the requests given as JSON argv entries
// sequentially and prints each response as JSON. Reads {port, token} from
// ~/.armada/server.json (legacy store still in use on this machine).
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const cfg = JSON.parse(readFileSync(join(homedir(), ".armada", "server.json"), "utf8"));
const requests = process.argv.slice(2).map((a) => JSON.parse(a));

const ws = new WebSocket(`ws://127.0.0.1:${cfg.port}/ws?token=${cfg.token}`);
let id = 0;
let pendingId = null;

function sendNext() {
  const req = requests.shift();
  if (!req) {
    ws.close();
    process.exit(0);
  }
  pendingId = ++id;
  ws.send(JSON.stringify({ id: pendingId, type: req.type, payload: req.payload ?? {} }));
}

ws.onopen = sendNext;
ws.onmessage = (m) => {
  const frame = JSON.parse(m.data);
  if (frame.type === "response" && frame.id === pendingId) {
    console.log(JSON.stringify(frame));
    sendNext();
  }
};
ws.onerror = (e) => {
  console.error("WS error:", e.message ?? e);
  process.exit(1);
};
setTimeout(() => {
  console.error("timeout");
  process.exit(2);
}, 120000);
