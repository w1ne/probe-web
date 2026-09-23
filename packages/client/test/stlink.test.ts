import { describe, expect, it } from 'vitest';
import { classifyStlinkInterfaceError } from '../src/stlink.ts';

describe('classifyStlinkInterfaceError', () => {
  it('maps an ST-Link endpoint failure and leaves CMSIS-DAP v1 alone', () => {
    const error = Object.assign(new Error('USB endpoint not found (EndpointNotFound)'), {
      kind: 'open-failed',
    });
    const classified = classifyStlinkInterfaceError(0x0483, error) as Error & { kind?: string };
    expect(classified.kind).toBe('stlink-interface');
    expect(classified.message).not.toMatch(/CMSIS-DAP v1/);
  });

  it('does not reclassify a DAPLink open failure', () => {
    const error = Object.assign(new Error('USB endpoint not found'), { kind: 'open-failed' });
    const classified = classifyStlinkInterfaceError(0x0d28, error) as Error & { kind?: string };
    expect(classified.kind).toBe('open-failed');
  });

  it('does not reclassify an unrelated ST-Link failure', () => {
    const error = Object.assign(new Error('permission denied'), { kind: 'open-failed' });
    const classified = classifyStlinkInterfaceError(0x0483, error) as Error & { kind?: string };
    expect(classified.kind).toBe('open-failed');
  });
});
