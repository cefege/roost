// The outbox intentionally stores rendered email content only as authenticated
// ciphertext. The recipient remains a routing column, but links/tokens and all
// email body content stay inside this envelope.

import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";

const FORMAT_VERSION = "v1";
const IV_BYTES = 12;
const AUTH_TAG_BYTES = 16;
const MAX_ENCODED_PAYLOAD_LENGTH = 2 * 1024 * 1024;

export interface EmailOutboxPayload {
  subject: string;
  html: string;
  text?: string;
}

export interface EmailOutboxPayloadContext {
  outboxId: string;
  kind: string;
}

export interface EmailOutboxPayloadCipher {
  encrypt(context: EmailOutboxPayloadContext, payload: EmailOutboxPayload): string;
  decrypt(context: EmailOutboxPayloadContext, encryptedPayload: string): EmailOutboxPayload;
}

/** Safe, intentionally detail-free error used at the dispatch boundary. */
export class EmailOutboxPayloadError extends Error {
  constructor() {
    super("invalid encrypted email outbox payload");
  }
}

function configuredKey(raw: string): Buffer {
  const base64 = /^[A-Za-z0-9+/]{43}=?$/.test(raw);
  const base64url = /^[A-Za-z0-9_-]{43}$/.test(raw);
  if (!base64 && !base64url) throw new Error("invalid email outbox key");

  const key = Buffer.from(raw, base64 ? "base64" : "base64url");
  const canonical = base64 ? key.toString("base64").replace(/=$/, "") : key.toString("base64url");
  if (key.byteLength !== 32 || canonical !== raw.replace(/=$/, "")) {
    throw new Error("invalid email outbox key");
  }
  return key;
}

function associatedData(context: EmailOutboxPayloadContext): Buffer {
  if (context.outboxId.length === 0 || context.kind.length === 0) {
    throw new EmailOutboxPayloadError();
  }
  return Buffer.from(`roost-email-outbox:${FORMAT_VERSION}:${context.outboxId}:${context.kind}`, "utf8");
}

function validPayload(value: unknown): value is EmailOutboxPayload {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const payload = value as Record<string, unknown>;
  return typeof payload.subject === "string"
    && payload.subject.length > 0
    && typeof payload.html === "string"
    && payload.html.length > 0
    && (payload.text === undefined || typeof payload.text === "string");
}

function decodeEnvelope(encryptedPayload: string): { iv: Buffer; tag: Buffer; ciphertext: Buffer } {
  if (encryptedPayload.length === 0 || encryptedPayload.length > MAX_ENCODED_PAYLOAD_LENGTH) {
    throw new EmailOutboxPayloadError();
  }
  const parts = encryptedPayload.split(".");
  if (parts.length !== 4 || parts[0] !== FORMAT_VERSION) throw new EmailOutboxPayloadError();
  const [, ivPart, tagPart, ciphertextPart] = parts;
  if (!ivPart || !tagPart || !ciphertextPart) throw new EmailOutboxPayloadError();

  try {
    const iv = Buffer.from(ivPart, "base64url");
    const tag = Buffer.from(tagPart, "base64url");
    const ciphertext = Buffer.from(ciphertextPart, "base64url");
    if (iv.byteLength !== IV_BYTES || tag.byteLength !== AUTH_TAG_BYTES || ciphertext.byteLength === 0) {
      throw new EmailOutboxPayloadError();
    }
    return { iv, tag, ciphertext };
  } catch (error) {
    if (error instanceof EmailOutboxPayloadError) throw error;
    throw new EmailOutboxPayloadError();
  }
}

/**
 * Builds the only encryption/decryption boundary for persisted email content.
 * The outbox row ID and message kind are AES-GCM additional authenticated data
 * so encrypted content cannot be copied to another row or message class.
 */
export function createEmailOutboxPayloadCipher(outboxKey: string): EmailOutboxPayloadCipher {
  const key = configuredKey(outboxKey);

  return {
    encrypt(context, payload): string {
      if (!validPayload(payload)) throw new EmailOutboxPayloadError();
      const plaintext = Buffer.from(JSON.stringify(payload), "utf8");
      const iv = randomBytes(IV_BYTES);
      const cipher = createCipheriv("aes-256-gcm", key, iv, { authTagLength: AUTH_TAG_BYTES });
      cipher.setAAD(associatedData(context));
      const ciphertext = Buffer.concat([cipher.update(plaintext), cipher.final()]);
      const tag = cipher.getAuthTag();
      return `${FORMAT_VERSION}.${iv.toString("base64url")}.${tag.toString("base64url")}.${ciphertext.toString("base64url")}`;
    },

    decrypt(context, encryptedPayload): EmailOutboxPayload {
      try {
        const { iv, tag, ciphertext } = decodeEnvelope(encryptedPayload);
        const decipher = createDecipheriv("aes-256-gcm", key, iv, { authTagLength: AUTH_TAG_BYTES });
        decipher.setAAD(associatedData(context));
        decipher.setAuthTag(tag);
        const plaintext = Buffer.concat([decipher.update(ciphertext), decipher.final()]);
        const parsed = JSON.parse(plaintext.toString("utf8")) as unknown;
        if (!validPayload(parsed)) throw new EmailOutboxPayloadError();
        return parsed;
      } catch (error) {
        if (error instanceof EmailOutboxPayloadError) throw error;
        throw new EmailOutboxPayloadError();
      }
    },
  };
}
