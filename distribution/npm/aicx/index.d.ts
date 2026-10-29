export declare function getBinaryPath(binaryName: "aicx" | "aicx-mcp"): string;

export declare function execAicx(args?: string[], options?: Record<string, unknown>): Buffer;
export declare function execAicxSync(args?: string[]): string;

export declare function execAicxMcp(args?: string[], options?: Record<string, unknown>): Buffer;
export declare function execAicxMcpSync(args?: string[]): string;

export interface PlatformHost {
  platform: string;
  arch: string;
  libc: "glibc" | "musl" | null;
}

export interface PlatformPackage {
  name: string;
  label: string;
}

export declare const PLATFORM_PACKAGES: Readonly<Record<string, Readonly<PlatformPackage>>>;
export declare function detectHost(): PlatformHost;
export declare function platformKeyFor(host: PlatformHost): string | null;
export declare function getPlatformPackageName(): string;

/** Raised when no prebuilt binary exists for the host or its platform package is absent. */
export declare class AicxPlatformError extends Error {
  exitCode: number;
}

/** Run a shipped binary as the bin/ shims do; platform errors print to stderr and exit non-zero. */
export declare function runCli(binaryName: "aicx" | "aicx-mcp", args?: string[]): void;
