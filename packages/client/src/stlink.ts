/** STMicroelectronics VID. ST-Link V2 and V2-1/V3 share it. */
const STLINK_VID = 0x0483;

/**
 * The shipped worker wasm still reports an unclaimed debug interface as
 * `open-failed`. Map that to `stlink-interface` so the dock does not reuse the
 * CMSIS-DAP v1 (HID) copy. A rebuilt worker that already sets the kind is unchanged.
 */
export function classifyStlinkInterfaceError(vendorId: number, error: unknown): unknown {
  if (!stlinkInterfaceFailure(vendorId, errorText(error))) return error;
  if (error instanceof Error) {
    (error as Error & { kind?: string }).kind = 'stlink-interface';
    return error;
  }
  return Object.assign(new Error(errorText(error)), { kind: 'stlink-interface' });
}

function stlinkInterfaceFailure(vendorId: number, message: string): boolean {
  if (vendorId !== STLINK_VID) return false;
  const text = message.toLowerCase();
  return (
    text.includes('endpoint not found') ||
    text.includes('endpointnotfound') ||
    text.includes('interface not found') ||
    text.includes('claiminterface') ||
    text.includes('could not be claimed')
  );
}

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
