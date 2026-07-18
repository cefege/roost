-- push_subscriptions: Web Push subscriptions, one row per device/browser.
-- viewer_fp is the browser's EdDSA fingerprint (kid hex, same identity used for
-- JWT auth). endpoint is the push service URL; unique per subscription. p256dh +
-- auth are the RFC 8291 encryption keys the coord needs to encrypt payloads.
CREATE TABLE IF NOT EXISTS push_subscriptions (
  viewer_fp    TEXT NOT NULL,           -- browser EdDSA fingerprint (kid hex)
  endpoint     TEXT NOT NULL,           -- push service URL; unique per subscription
  p256dh       TEXT NOT NULL,           -- base64url client ECDH public key
  auth         TEXT NOT NULL,           -- base64url auth secret
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (viewer_fp, endpoint)
);
