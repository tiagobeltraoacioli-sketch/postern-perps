import { afterEach } from 'vitest';
import { cleanup } from '@testing-library/react';
import { FakeCtx } from './fake-canvas';

// jsdom has no 2D canvas: every canvas gets its own recording context, reachable as canvas.__fake.
Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
  configurable: true,
  value(this: HTMLCanvasElement & { __fake?: FakeCtx }) {
    this.__fake ??= new FakeCtx();
    return this.__fake;
  },
});

afterEach(() => cleanup());
