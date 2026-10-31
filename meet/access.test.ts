// Made-up keys only: none of these is, or is like, a real host key.
import { describe, expect, it } from 'vitest';
import { hostKeys, refusal } from './access';

describe('hostKeys', () => {
  it('has none without HOST_KEYS or HOST_KEY', () => {
    expect(hostKeys({})).toEqual([]);
    expect(hostKeys({ HOST_KEYS: '', HOST_KEY: '' })).toEqual([]);
  });

  it('takes one name:key a line', () => {
    expect(hostKeys({ HOST_KEYS: 'ada:test-key-one\nbo:test-key-two' })).toEqual([
      'test-key-one',
      'test-key-two',
    ]);
  });

  it('takes entries separated by commas', () => {
    expect(hostKeys({ HOST_KEYS: 'ada:test-key-one,bo:test-key-two' })).toEqual([
      'test-key-one',
      'test-key-two',
    ]);
  });

  it('takes a bare key without a name', () => {
    expect(hostKeys({ HOST_KEYS: 'test-key-bare' })).toEqual(['test-key-bare']);
  });

  it('keeps the colons in a key after its name', () => {
    expect(hostKeys({ HOST_KEYS: 'ada:test:key' })).toEqual(['test:key']);
  });

  it('skips blank entries and trims spaces and a trailing newline', () => {
    expect(
      hostKeys({ HOST_KEYS: '\n ada: test-key-one \r\n\n, ,bo:test-key-two\n', HOST_KEY: ' ' }),
    ).toEqual(['test-key-one', 'test-key-two']);
  });

  it('adds HOST_KEY, from before there were several, last', () => {
    expect(hostKeys({ HOST_KEYS: 'ada:test-key-one', HOST_KEY: 'test-key-old\n' })).toEqual([
      'test-key-one',
      'test-key-old',
    ]);
    expect(hostKeys({ HOST_KEY: 'test-key-old' })).toEqual(['test-key-old']);
  });

  it('skips a name with no key', () => {
    expect(hostKeys({ HOST_KEYS: 'ada:\nbo:test-key-two' })).toEqual(['test-key-two']);
  });
});

describe('refusal', () => {
  const keys = ['test-key-one', 'test-key-two'];

  it('lets a host key open a room no one is streaming in', async () => {
    expect(await refusal('Bearer test-key-one', false, keys)).toBeNull();
    expect(await refusal('Bearer test-key-two', false, keys)).toBeNull();
  });

  it('refuses a wrong key with exactly the words the app looks for', async () => {
    // shuttercrab-connect's NOT_THE_HOST_KEY.
    expect(await refusal('Bearer test-key-wrong', false, keys)).toEqual([
      4401,
      'Not the host key',
    ]);
  });

  it('refuses a wrong key even when someone is streaming', async () => {
    expect(await refusal('Bearer test-key-wrong', true, keys)).toEqual([
      4401,
      'Not the host key',
    ]);
  });

  it('refuses a key that only starts or ends like a host key', async () => {
    expect(await refusal('Bearer test-key-on', false, keys)).toEqual([4401, 'Not the host key']);
    expect(await refusal('Bearer test-key-one2', false, keys)).toEqual([
      4401,
      'Not the host key',
    ]);
  });

  it('refuses a header that is not a Bearer key', async () => {
    expect(await refusal('test-key-one', false, keys)).toEqual([4401, 'Not the host key']);
    expect(await refusal('Basic test-key-one', false, keys)).toEqual([4401, 'Not the host key']);
    expect(await refusal('bearer test-key-one', false, keys)).toEqual([4401, 'Not the host key']);
    expect(await refusal('', false, keys)).toEqual([4401, 'Not the host key']);
  });

  it('refuses every key when there are no host keys', async () => {
    expect(await refusal('Bearer ', false, [])).toEqual([4401, 'Not the host key']);
    expect(await refusal('Bearer test-key-one', false, [])).toEqual([
      4401,
      'Not the host key',
    ]);
  });

  it('refuses a second host while someone is streaming', async () => {
    expect(await refusal('Bearer test-key-two', true, keys)).toEqual([
      4409,
      'Someone is already streaming here',
    ]);
  });

  it('lets a viewer join a room someone is streaming in', async () => {
    expect(await refusal(null, true, keys)).toBeNull();
  });

  it('refuses a viewer when no one is streaming', async () => {
    expect(await refusal(null, false, keys)).toEqual([4404, 'No one is streaming here']);
  });
});
