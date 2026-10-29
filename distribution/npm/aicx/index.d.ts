export declare function getBinaryPath(binaryName: "aicx" | "aicx-mcp"): string;

export declare function execAicx(args?: string[], options?: Record<string, unknown>): Buffer;
export declare function execAicxSync(args?: string[]): string;

export declare function execAicxMcp(args?: string[], options?: Record<string, unknown>): Buffer;
export declare function execAicxMcpSync(args?: string[]): string;

export declare class AicxPlatformError extends Error {
  readonly exitCode: number;
}

export interface AicxHost {
  platform?: string;
  arch?: string;
  libc?: "glibc" | "musl" | string | null;
}

export declare const PLATFORM_PACKAGES: Readonly<Record<string, Readonly<{ name: string; label: string }>>>;

export declare function platformKeyFor(host: Required<AicxHost>): string;
export declare function getPlatformKey(host?: AicxHost): string;
export declare function getPlatformPackage(host?: AicxHost): {
  name: string;
  label: string;
  key: string;
  host: { platform: string; arch: string; libc: string | null };
};
export declare function getPlatformPackageName(): string;
export declare function runCli(binaryName: "aicx" | "aicx-mcp", args?: string[]): Buffer;
