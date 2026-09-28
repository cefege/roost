import pathlib
import re

T = pathlib.Path("/home/mike/repos/roost-v3-worker/target-track/tmp/wpeer-fmt/tests")
OUT = pathlib.Path("/home/mike/repos/roost-v3-worker/target-track/tmp/wpeer-split")
(OUT / "peer_support").mkdir(parents=True, exist_ok=True)

PATHS = {
    "std::sync": ["Arc", "Mutex", "MutexGuard", "PoisonError"],
    "std::time": ["Instant", "Duration"],
    "roost_proto": ["DLocalTerminalPeerCancel", "DLocalTerminalPeerOffer"],
    "roost_protocol::terminal_peer::packets": [
        "TerminalPeerPacketHeader", "TerminalPeerPacketQuota", "encode_terminal_peer_packet",
        "parse_terminal_peer_packet"],
    "roost_protocol::terminal_peer::peer": [
        "TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES", "TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES",
        "TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN", "TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES",
        "TerminalPeerChannelWatermarks"],
    "roost_worker::local_terminal": [
        "ExpectedPeer", "PeerGrantAuthorization", "PacketSendResult", "PeerTerminalPacketPort",
        "TerminalPacketPort"],
    "roost_worker::peer::native": ["NativeLoader", "NativePeerEvent"],
    "roost_worker::peer": [
        "OfferFault", "OfferFaultSlot", "PeerBootstrapState", "PeerTransportConfig",
        "TerminalPeerOfferFailure", "TerminalPeerOwner", "TerminalPeerOwnerDeps",
        "TerminalPeerPacketBudget", "TerminalPeerPacketIngress", "TerminalPeerPacketPort",
        "PacketDirection", "PacketPortDeps"],
    "roost_worker::uplink": ["RequestBudget", "Uplink"],
    "tokio::sync": ["Semaphore"],
}
FUNCTIONS = {"lock", "uuid", "peer_offer", "owner_with", "budget", "offer", "settle", "fixture_with", "fixture", "framed_control", "offer_sdp"}
LANE_ALIAS = "TerminalPeerPacketLane as Lane"


def uses(body, extra):
    lines = []
    groups = [("std", []), ("ext", []), ("crate", [])]
    for path, names in list(PATHS.items()) + extra:
        found = [n for n in names if re.search(r"\b" + re.escape(n) + (r"\(" if n in FUNCTIONS else r"\b"), body)]
        if path == "roost_protocol::terminal_peer::packets" and re.search(r"\bLane::", body):
            found.append(LANE_ALIAS)
        if not found:
            continue
        line = f"use {path}::{{{', '.join(found)}}};" if len(found) > 1 else f"use {path}::{found[0]};"
        if path.startswith("std"):
            groups[0][1].append(line)
        elif path.startswith("crate"):
            groups[2][1].append(line)
        else:
            groups[1][1].append(line)
    return "\n\n".join("\n".join(g) for _, g in groups if g)


def publicize(text):
    text = re.sub(r"^(const|fn|async fn|struct) ", r"pub \1 ", text, flags=re.M)
    out, block = [], None
    for line in text.split("\n"):
        if re.match(r"^pub struct \w+ \{$", line):
            block = "struct"
        elif re.match(r"^impl \w+ \{$", line):
            block = "impl"
        elif line == "}":
            block = None
        elif block == "struct":
            line = re.sub(r"^    ([a-z_]+): ", r"    pub \1: ", line)
        elif block == "impl":
            line = re.sub(r"^    fn ", "    pub fn ", line)
        out.append(line)
    return "\n".join(out)


def cut(lines, start, end):
    return "\n".join(lines[start - 1:end]) + "\n"


# ---- owner ----
owner = (T / "terminal_peer_owner.rs").read_text().split("\n")
helpers = cut(owner, 26, 129)
tests_main = cut(owner, 131, 389)
tests_faults = cut(owner, 391, 429)
fixture_names = ["WORKER_EPOCH", "DEVICE", "lock", "NoIngress", "uuid", "peer_offer", "Seen",
                 "owner_with", "budget", "offer", "settle"]
helpers_pub = publicize(helpers).replace("pub struct NoIngress;", "pub struct NoIngress;")
(OUT / "peer_support/owner_fixture.rs").write_text(
    "//! The terminal peer owner over the deterministic native fake, and the\n"
    "//! records its injected grant, expiry and port dependencies keep. Included by\n"
    "//! `terminal_peer_owner.rs` and `terminal_peer_offer_faults.rs`. Ports v2\n"
    "//! `apps/worker/tests/terminal/peer/terminal-peer-owner-fixture.ts`.\n"
    "#![allow(dead_code)]\n\n"
    + uses(helpers, [("crate::fake_native", ["FakeNative", "offer_sdp"])]) + "\n\n" + helpers_pub)

head = ("#![allow(clippy::unwrap_used, clippy::expect_used)]\n\n"
        "#[path = \"peer_support/fake_native.rs\"]\nmod fake_native;\n"
        "#[path = \"peer_support/owner_fixture.rs\"]\nmod owner_fixture;\n\n")
owner_extra = [("fake_native", ["FakeNative", "offer_sdp"]), ("owner_fixture", fixture_names)]
(OUT / "terminal_peer_owner.rs").write_text(
    "//! Terminal peer owner: offer fencing, bounded negotiations, the expected\n"
    "//! tuple bound before native SDP, and single-peer retirement — over the\n"
    "//! deterministic native fake. Ports v2\n"
    "//! `apps/worker/tests/terminal/peer/terminal-peer-owner.test.ts`.\n"
    + head + uses(tests_main, owner_extra) + "\n\n" + tests_main)
(OUT / "terminal_peer_offer_faults.rs").write_text(
    "//! The smoke offer faults at their real owner boundaries: each armed fault\n"
    "//! is consumed by exactly the next offer and leaves the one after untouched.\n"
    "//! Ports the offer injection sites of v2\n"
    "//! `apps/worker/src/terminal/peer/terminal-peer-test-faults.ts`.\n"
    + head + uses(tests_faults, owner_extra) + "\n\n" + tests_faults)

# ---- packet port ----
port = (T / "terminal_peer_packet_port.rs").read_text().split("\n")
p_helpers = cut(port, 31, 126)
starts = [i + 1 for i, line in enumerate(port) if line == "#[tokio::test]"]
docs = []
for start in starts:
    doc = start
    while doc - 1 >= 1 and port[doc - 2].startswith("///"):
        doc -= 1
    docs.append(doc)
blocks = []
for index, doc in enumerate(docs):
    end = docs[index + 1] - 1 if index + 1 < len(docs) else len(port)
    blocks.append((doc, end))
ingress_names = ("reassembles_framed_control", "refuses_non_control_client_data")
egress, ingress = [], []
for doc, end in blocks:
    text = cut(port, doc, end).rstrip("\n") + "\n"
    (ingress if any(name in text for name in ingress_names) else egress).append(text)
p_fixture_names = ["lock", "Recorded", "RecordingIngress", "Fixture", "fixture_with", "fixture",
                   "framed_control"]
(OUT / "peer_support/packet_port_fixture.rs").write_text(
    "//! A terminal peer packet port over three fake native channels, its ingress\n"
    "//! recorded, and v2's channel helpers (`emitLow`, saturation, framed\n"
    "//! control). Included by `terminal_peer_packet_port.rs` and\n"
    "//! `terminal_peer_packet_ingress.rs`. Ports the fixture of v2\n"
    "//! `apps/worker/tests/terminal/peer/terminal-peer-packet-port.test.ts`.\n"
    "#![allow(dead_code)]\n\n"
    + uses(p_helpers, [("crate::fake_native", ["FakePeer"])]) + "\n\n" + publicize(p_helpers)
    .replace("pub struct RecordingIngress(Arc<Recorded>);", "pub struct RecordingIngress(pub Arc<Recorded>);"))
p_head = ("#![allow(clippy::unwrap_used, clippy::expect_used)]\n\n"
          "#[path = \"peer_support/fake_native.rs\"]\nmod fake_native;\n"
          "#[path = \"peer_support/packet_port_fixture.rs\"]\nmod packet_port_fixture;\n\n")
p_extra = [("fake_native", ["FakePeer"]), ("packet_port_fixture", p_fixture_names)]
egress_body = "\n".join(egress)
ingress_body = "\n".join(ingress)
(OUT / "terminal_peer_packet_port.rs").write_text(
    "//! Terminal peer packet port egress over fake native channels: ownership at\n"
    "//! the native false-send boundary, the control reservation that keeps\n"
    "//! history from blocking control, lane priority, the per-turn flush bound,\n"
    "//! history drain and pre-read reservation, and pressure retirement. Ports v2\n"
    "//! `apps/worker/tests/terminal/peer/terminal-peer-packet-port.test.ts`.\n"
    + p_head + uses(egress_body, p_extra) + "\n\n" + egress_body)
(OUT / "terminal_peer_packet_ingress.rs").write_text(
    "//! Terminal peer packet port ingress: framed control reassembly, malformed\n"
    "//! packet rejection with its partial quota released on close, and refusal of\n"
    "//! client data off the control lane. Ports the receive cases of v2\n"
    "//! `apps/worker/tests/terminal/peer/terminal-peer-packet-port.test.ts`.\n"
    + p_head + uses(ingress_body, p_extra) + "\n\n" + ingress_body)
print("egress", len(egress), "ingress", len(ingress))
