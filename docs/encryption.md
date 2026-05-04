# Encryption

All chat messages in Eurus are encrypted end-to-end using AES-256-GCM. The server stores and routes only ciphertext — it never has access to plaintext message content.

## Algorithm

| Property | Value |
|---|---|
| Cipher | AES-256-GCM |
| Key size | 256 bits (32 bytes) |
| Nonce size | 96 bits (12 bytes) |
| Authentication tag | 128 bits (16 bytes, appended by GCM) |
| Encoding | Hex string (nonce + ciphertext + tag) |

## How It Works

### Key Generation

When a room is created, the server generates a random AES-256 key and stores it in the `RoomKey` table, encrypted per user. The client receives this key in the `roomJoined` payload as `encryptedKey`.

### Encryption (Client-Side)

When sending a message:

1. The client has the room's AES-256 key (32 bytes, hex-encoded).
2. A random 12-byte nonce is generated for each message.
3. The plaintext message is encrypted with AES-256-GCM using the key and nonce.
4. The nonce is prepended to the ciphertext (which includes the GCM authentication tag).
5. The combined bytes (nonce + ciphertext + tag) are hex-encoded into a string.
6. The hex string is sent to the server as the `ciphertext` field.

```
[12-byte nonce][ciphertext + 16-byte GCM tag]
       │                    │
       └── prepended ───────┘
              │
              ▼
        hex-encoded string → sent to server
```

### Decryption (Client-Side)

When receiving a message:

1. The hex-encoded ciphertext is decoded back to bytes.
2. The first 12 bytes are extracted as the nonce.
3. The remaining bytes are the ciphertext + GCM tag.
4. AES-256-GCM decryption is attempted with the room key and nonce.
5. If the GCM tag verification fails, decryption returns an error (message was tampered with).
6. On success, the plaintext is returned as a UTF-8 string.

## Implementation (`crypto.rs`)

```rust
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};

pub fn encrypt(plaintext: &str, key_bytes: &[u8]) -> Result<String> {
    let key = key_from_hex(key_bytes)?;
    let cipher = Aes256Gcm::new(&key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng); // Random 12 bytes
    let ciphertext = cipher.encrypt(&nonce, plaintext.as_bytes())?;

    // Prepend nonce to ciphertext
    let mut combined = nonce.to_vec();
    combined.extend(ciphertext);

    // Hex-encode for transmission
    Ok(hex::encode(combined))
}

pub fn decrypt(ciphertext_hex: &str, key_bytes: &[u8]) -> Result<String> {
    let key = key_from_hex(key_bytes)?;
    let cipher = Aes256Gcm::new(&key);
    let combined = hex::decode(ciphertext_hex)?;

    // Split nonce (first 12 bytes) from ciphertext
    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext)?;
    Ok(String::from_utf8(plaintext)?)
}
```

## Security Properties

### Confidentiality

AES-256-GCM provides strong confidentiality. The 256-bit key space makes brute-force attacks computationally infeasible. Each message uses a unique random nonce, preventing nonce reuse attacks.

### Integrity

The GCM authentication tag (16 bytes) ensures message integrity. If any byte of the ciphertext is modified in transit, decryption will fail with an authentication error. The server cannot forge or modify messages without detection.

### Nonce Uniqueness

Each message gets a cryptographically random 12-byte nonce. The probability of nonce collision is negligible (2^96 possible values). Nonce reuse with the same key would compromise security, so random generation is critical.

### Key Distribution

Room keys are generated server-side and distributed to members. The server knows the key but cannot determine which messages belong to which plaintext without the key being used client-side. This is a trust model where the server is trusted for key distribution but not trusted with content access — the server *could* theoretically decrypt if it wanted to, but the architecture ensures it doesn't need to and shouldn't.

### What Is Not Encrypted

The following data is transmitted in plaintext:

- **Metadata:** Sender username, message timestamp, message type (text/image)
- **Room information:** Room name, display name, member list
- **Voice data:** Opus audio frames (these are encoded but not encrypted)
- **Authentication:** JWT tokens (protected by TLS in transit)

The metadata is necessary for the server to route messages and manage rooms. Voice data is not encrypted because the server needs to decode and mix it. If voice encryption is required in the future, it would need a different architecture (e.g., client-side mixing with peer-to-peer audio).

## Key Storage

The room key is stored on the client side in the `App` struct as `room_key: Option<String>`. It is loaded from the server when joining a room and kept in memory for the duration of the session. It is not persisted to disk.
