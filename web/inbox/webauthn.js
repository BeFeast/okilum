export function decode(value) {
  const binary = atob(value.replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(binary, c => c.charCodeAt(0)).buffer;
}
export function encode(value) {
  return btoa(String.fromCharCode(...new Uint8Array(value)))
    .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
export function creationOptions(options) {
  const copy = structuredClone(options);
  copy.publicKey.challenge = decode(copy.publicKey.challenge);
  copy.publicKey.user.id = decode(copy.publicKey.user.id);
  for (const credential of copy.publicKey.excludeCredentials || []) credential.id = decode(credential.id);
  return copy;
}
export function requestOptions(options) {
  const copy = structuredClone(options);
  copy.publicKey.challenge = decode(copy.publicKey.challenge);
  for (const credential of copy.publicKey.allowCredentials || []) credential.id = decode(credential.id);
  return copy;
}
export function credentialJSON(credential) {
  if (!credential) throw new Error('Passkey request was cancelled.');
  const r = credential.response;
  const response = { clientDataJSON: encode(r.clientDataJSON) };
  if (r.attestationObject) {
    response.attestationObject = encode(r.attestationObject);
    response.transports = r.getTransports?.() || [];
  } else {
    response.authenticatorData = encode(r.authenticatorData);
    response.signature = encode(r.signature);
    response.userHandle = r.userHandle ? encode(r.userHandle) : null;
  }
  return { id: credential.id, rawId: encode(credential.rawId), type: credential.type,
    response, extensions: credential.getClientExtensionResults() };
}
