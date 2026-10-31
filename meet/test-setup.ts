// Node's Web Crypto has no `timingSafeEqual`, which the Workers runtime adds
// to `crypto.subtle` and the host key check uses: the same comparison here,
// for the tests only.
if (!('timingSafeEqual' in crypto.subtle)) {
  Object.defineProperty(crypto.subtle, 'timingSafeEqual', {
    value(a: ArrayBuffer | ArrayBufferView, b: ArrayBuffer | ArrayBufferView): boolean {
      const bytes = (v: ArrayBuffer | ArrayBufferView) =>
        ArrayBuffer.isView(v)
          ? new Uint8Array(v.buffer, v.byteOffset, v.byteLength)
          : new Uint8Array(v);
      const x = bytes(a);
      const y = bytes(b);
      if (x.length !== y.length) {
        throw new TypeError('Input buffers must have the same byte length');
      }
      let difference = 0;
      for (let i = 0; i < x.length; i++) {
        difference |= x[i]! ^ y[i]!;
      }
      return difference === 0;
    },
  });
}
