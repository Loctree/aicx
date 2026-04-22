export declare function getBinaryPath(binaryName: "aicx" | "aicx-mcp"): string;

export declare function execAicx(args?: string[], options?: Record<string, unknown>): Buffer;
export declare function execAicxSync(args?: string[]): string;

export declare function execAicxMcp(args?: string[], options?: Record<string, unknown>): Buffer;
export declare function execAicxMcpSync(args?: string[]): string;
