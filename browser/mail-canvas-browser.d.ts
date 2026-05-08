export type MailCanvasAssetLimits = {
  maxAssetBytes?: number;
  maxTotalAssetBytes?: number;
  maxAssetCount?: number;
};

export type MailCanvasFontInput =
  | URL
  | string
  | {
      url?: string;
      bytes: Uint8Array | ArrayBuffer | ArrayBufferView;
    };

export type CreateMailCanvasRendererOptions = {
  workerUrl: string | URL;
  baseUrl?: string | URL;
  fonts?: MailCanvasFontInput[];
  defaultEmojiFont?: MailCanvasFontInput | false;
  limits?: MailCanvasAssetLimits;
};

export type RenderThumbnailOptions = {
  html: string;
  width?: number;
  height?: number;
  viewportHeight?: number;
  scale?: number;
  baseUrl?: string | URL;
};

export type RenderDiagnostics = {
  warnings: unknown[];
  assets: unknown[];
  console_messages: unknown[];
};

export type RenderThumbnailResult = {
  png: Uint8Array;
  blob: Blob;
  width: number;
  height: number;
  scale: number;
  diagnostics: RenderDiagnostics;
  assets: unknown;
  timing: {
    fetchMs: number;
    renderMs: number;
    totalMs: number;
  };
};

export declare function createMailCanvasRenderer(
  options: CreateMailCanvasRendererOptions,
): Promise<MailCanvasBrowserRenderer>;

export declare class MailCanvasBrowserRenderer {
  renderThumbnail(options: RenderThumbnailOptions): Promise<RenderThumbnailResult>;
  clearCache(): Promise<void>;
  destroy(): void;
}
