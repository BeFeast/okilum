import test from 'node:test';
import assert from 'node:assert/strict';
import { encode, decode, creationOptions, requestOptions, credentialJSON } from '../webauthn.js';
test('base64url binary fields survive registration and assertion conversion', () => {
  const binary = new Uint8Array([0, 255, 238, 20, 0]).buffer;
  const encoded = encode(binary);
  assert.deepEqual(decode(encoded), binary);
  const create = { publicKey: { challenge: encoded, user: { id: encoded }, excludeCredentials: [{ id: encoded }] } };
  assert.deepEqual(creationOptions(create).publicKey.challenge, binary);
  assert.deepEqual(creationOptions(create).publicKey.user.id, binary);
  assert.equal(create.publicKey.challenge, encoded);
  const get = { publicKey: { challenge: encoded, allowCredentials: [{ id: encoded }] } };
  assert.deepEqual(requestOptions(get).publicKey.allowCredentials[0].id, binary);
  const credential = { id: encoded, rawId: binary, type: 'public-key', getClientExtensionResults: () => ({}), response: { clientDataJSON: binary, attestationObject: binary, getTransports: () => ['internal'] } };
  const registration = credentialJSON(credential);
  assert.equal(registration.response.attestationObject, encoded);
  assert.deepEqual(registration.response.transports, ['internal']);
  credential.response = { clientDataJSON: binary, authenticatorData: binary, signature: binary, userHandle: null };
  const assertion = credentialJSON(credential);
  assert.equal(assertion.response.signature, encoded);
  assert.equal(assertion.response.userHandle, null);
  assert.throws(() => credentialJSON(null));
});

test('optional credential properties do not turn a valid browser response into a parse error', () => {
 const binary = new Uint8Array([1,2]).buffer;
 const credential = {id:'AQI',rawId:binary,type:'public-key',response:{clientDataJSON:binary,attestationObject:binary}};
 for (const rk of [undefined,false,true]) {
  const extensions={credProps:rk===undefined?{}:{rk}};
  const result=credentialJSON({...credential,getClientExtensionResults:()=>extensions});
  assert.deepEqual(result.extensions,rk===undefined?{}:{credProps:{rk}});
  assert.deepEqual(extensions,{credProps:rk===undefined?{}:{rk}});
 }
});
