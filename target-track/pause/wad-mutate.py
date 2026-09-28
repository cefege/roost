import subprocess, pathlib, sys, os
R = pathlib.Path('/home/almalinux/repos/roost-v3-worker/crates')
W = R / 'roost-worker/src'
M = [
 (W/'attachments/direct_hello.rs', 60, 'if replayed {', 'if false && replayed {', 'roost-worker', ['--test', 'attachment_direct_socket', 'a_replayed_grant']),
 (W/'attachments/direct_sockets.rs', 99, '|session| !session.holds_admitted_slot()', '|_session| true', 'roost-worker', ['--test', 'attachment_direct_socket', 'an_admitted_upload_frees']),
 (W/'attachments/direct_sockets.rs', 253, 'metadata.device_fingerprint == device_fingerprint', 'metadata.device_fingerprint != device_fingerprint', 'roost-worker', ['--test', 'attachment_direct_socket', 'explicit_device_revocation']),
 (W/'attachments/direct_chunks.rs', 227, 'send_attachment_closed(port.as_ref(), COMPLETE_REASON);', 'let _ = &port;', 'roost-worker', ['--test', 'attachment_direct_socket', 'writes_exact_bytes']),
 (W/'attachments/direct_hello.rs', 147, 'Duration::from_millis(HELLO_DEADLINE_MS)', 'Duration::from_millis(HELLO_DEADLINE_MS * 2)', 'roost-worker', ['--test', 'attachment_direct_socket', 'a_loopback_socket_that_never_says_hello']),
 (W/'attachments/peer_packet_port.rs', 182, 'if !state.authenticated && lane != PeerChannelLane::Control {', 'if false {', 'roost-worker', ['--test', 'attachment_peer_port', 'the_data_channel_is_closed']),
 (W/'attachments/peer_packet_port.rs', 140, '&& state.hello_timer.is_none()', '&& false', 'roost-worker', ['--test', 'attachment_peer_port', 'an_unadmitted_peer_is_retired']),
 (W/'attachments/peer_negotiation.rs', 243, 'state.active.remove(peer_id);', 'let _ = peer_id;', 'roost-worker', ['--test', 'attachment_peer_owner']),
 (W/'attachments/peer_budget.rs', 143, '&& !shared.reserve_worker_data(bytes)', '&& !shared.reserve_worker_data(0)', 'roost-worker', ['--lib', 'the_worker_wide_data_ceiling']),
 (R/'roost-protocol/src/attachment_transfer/packets.rs', 254, '|| header.offset_bytes as usize != partial.bytes.len()', '|| false', 'roost-protocol', ['--test', 'attachment_transfer_packets', 'rejects_terminal_magic']),
 (W/'attachments/peer_request_validation.rs', 28, "(b'1'..=b'5').contains(byte)", 'byte.is_ascii_hexdigit()', 'roost-worker', ['--lib', 'refuses_an_unversioned_peer']),
 (W/'runtime/downstream/attachment_peer.rs', 54, 'attachment_peer_error(&refused, &epoch, reason)', 'attachment_peer_error(&refused, &epoch, PeerErrorReason::IceFailed)', 'roost-worker', ['--test', 'link_downstream_attachment_peer', 'attachment_controls_emit']),
]
import signal
current = []
def restore(*_):
    for path, text in current:
        path.write_text(text)
    sys.exit(1)
signal.signal(signal.SIGTERM, restore)
signal.signal(signal.SIGHUP, restore)
only = set(int(a) for a in sys.argv[1:])
for idx, (path, line, old, new, pkg, args) in enumerate(M):
    if only and idx not in only:
        continue
    text = path.read_text()
    lines = text.split('\n')
    if old not in lines[line - 1]:
        print(f'M{idx} SKIP {path.name}:{line} anchor not found: {lines[line-1]!r}', flush=True)
        continue
    lines[line - 1] = lines[line - 1].replace(old, new)
    current.append((path, text))
    path.write_text('\n'.join(lines))
    try:
        run = subprocess.run([os.environ.get('WAD_CARGO', '/tmp/wcargo.sh'), 'test', '-p', pkg] + args, capture_output=True, text=True)
    finally:
        path.write_text(text)
        current.clear()
    out = run.stdout + run.stderr
    summary = [l for l in out.split('\n') if l.startswith('test ') and ('FAILED' in l or ' ok' in l) or 'test result' in l or l.startswith('error')]
    print(f'M{idx} {path.relative_to(R)}:{line} `{old}` -> `{new}` exit={run.returncode}', flush=True)
    for l in summary[:8]:
        print('   ', l, flush=True)
print('DONE', flush=True)
