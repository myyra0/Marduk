import { describe, expect, it } from 'vitest';

import { ProgressStream } from './progress';
import type { Phase, Progress } from './wire';

function hint(sequence: string, operationId = '7', phase: Phase = 'applying'): Progress {
  return { operationId, sequence, phase };
}

describe('progress ordering', () => {
  it('accepts strictly increasing sequences per operation', () => {
    const stream = new ProgressStream();
    expect(stream.observe(hint('1'))).toBe('advance');
    expect(stream.observe(hint('2'))).toBe('advance');
    expect(stream.observe(hint('3'))).toBe('advance');
  });

  it('reports duplicates and gaps without advancing the claim', () => {
    const stream = new ProgressStream();
    expect(stream.observe(hint('1'))).toBe('advance');
    expect(stream.observe(hint('1'))).toBe('duplicate');
    expect(stream.observe(hint('3'))).toBe('gap');
    expect(stream.observe(hint('4'))).toBe('advance');
  });

  it('tracks operations independently', () => {
    const stream = new ProgressStream();
    expect(stream.observe(hint('5', '7'))).toBe('advance');
    expect(stream.observe(hint('1', '8'))).toBe('advance');
    expect(stream.observe(hint('6', '7'))).toBe('advance');
  });
});
