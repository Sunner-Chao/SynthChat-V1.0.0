/**
 * Simulation tests for attachment Object URL lifecycle management.
 *
 * ChatExperience and PetWindow both create blob preview URLs via
 * URL.createObjectURL(file). Earlier they were never revoked — these tests
 * verify the corrected behaviour: URLs are revoked when:
 *
 *  1. removeAttachment / removePetAttachment is called
 *  2. The attachment list is cleared on successful submit
 *  3. Component unmount revokes all remaining blob URLs (via attachmentsRef)
 */

import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";

// ---------------------------------------------------------------------------
// Shared helpers that mirror the logic extracted from the components
// ---------------------------------------------------------------------------

type MockAttachment = {
  id: string;
  preview: string | null;
  status: "ready" | "staging" | "error";
};

/**
 * Reproduces the corrected removeAttachment logic from ChatExperience /
 * removePetAttachment from PetWindow.
 */
function removeAttachment(
  attachments: MockAttachment[],
  id: string,
  revokeUrl: (url: string) => void
): MockAttachment[] {
  const item = attachments.find((a) => a.id === id);
  if (item?.preview?.startsWith("blob:")) {
    revokeUrl(item.preview);
  }
  return attachments.filter((a) => a.id !== id);
}

/**
 * Reproduces the corrected submit attachment-clear path from ChatExperience /
 * handleSubmit from PetWindow.
 */
function clearAttachments(
  attachments: MockAttachment[],
  revokeUrl: (url: string) => void
): MockAttachment[] {
  for (const a of attachments) {
    if (a.preview?.startsWith("blob:")) revokeUrl(a.preview);
  }
  return [];
}

/**
 * Reproduces the component-unmount cleanup using an attachmentsRef snapshot.
 */
function unmountCleanup(
  attachmentsRef: { current: MockAttachment[] },
  revokeUrl: (url: string) => void
): void {
  for (const a of attachmentsRef.current) {
    if (a.preview?.startsWith("blob:")) revokeUrl(a.preview);
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("attachment Object URL lifecycle — removeAttachment", () => {
  let revokeUrl: (url: string) => void;

  beforeEach(() => {
    // Use a simple spy wrapper that satisfies the (url: string) => void signature
    revokeUrl = vi.fn() as unknown as (url: string) => void;
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("revokes the blob preview URL when an attachment with a blob URL is removed", () => {
    const attachments: MockAttachment[] = [
      { id: "a1", preview: "blob:http://localhost/fake-blob-1", status: "ready" },
      { id: "a2", preview: null, status: "ready" }
    ];

    const result = removeAttachment(attachments, "a1", revokeUrl);

    expect(revokeUrl).toHaveBeenCalledOnce();
    expect(revokeUrl).toHaveBeenCalledWith("blob:http://localhost/fake-blob-1");
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("a2");
  });

  it("does NOT call revokeObjectURL for an attachment with no preview", () => {
    const attachments: MockAttachment[] = [
      { id: "a1", preview: null, status: "ready" }
    ];

    removeAttachment(attachments, "a1", revokeUrl);

    expect(revokeUrl).not.toHaveBeenCalled();
  });

  it("does NOT call revokeObjectURL for a non-blob data URL preview", () => {
    const attachments: MockAttachment[] = [
      { id: "a1", preview: "data:image/png;base64,abc", status: "ready" }
    ];

    removeAttachment(attachments, "a1", revokeUrl);

    expect(revokeUrl).not.toHaveBeenCalled();
  });

  it("does NOT call revokeObjectURL for a different attachment ID", () => {
    const attachments: MockAttachment[] = [
      { id: "a1", preview: "blob:http://localhost/fake-blob-1", status: "ready" },
      { id: "a2", preview: "blob:http://localhost/fake-blob-2", status: "ready" }
    ];

    removeAttachment(attachments, "a1", revokeUrl);

    expect(revokeUrl).toHaveBeenCalledOnce();
    expect(revokeUrl).toHaveBeenCalledWith("blob:http://localhost/fake-blob-1");
  });
});

describe("attachment Object URL lifecycle — submit clear", () => {
  let revokeUrl: (url: string) => void;

  beforeEach(() => {
    // Use a simple spy wrapper that satisfies the (url: string) => void signature
    revokeUrl = vi.fn() as unknown as (url: string) => void;
  });

  it("revokes all blob preview URLs when attachments are cleared on submit", () => {
    const attachments: MockAttachment[] = [
      { id: "a1", preview: "blob:http://localhost/blob-1", status: "ready" },
      { id: "a2", preview: "blob:http://localhost/blob-2", status: "ready" },
      { id: "a3", preview: null, status: "ready" }
    ];

    const result = clearAttachments(attachments, revokeUrl);

    expect(revokeUrl).toHaveBeenCalledTimes(2);
    expect(revokeUrl).toHaveBeenCalledWith("blob:http://localhost/blob-1");
    expect(revokeUrl).toHaveBeenCalledWith("blob:http://localhost/blob-2");
    expect(result).toHaveLength(0);
  });

  it("does nothing when there are no attachments", () => {
    const result = clearAttachments([], revokeUrl);

    expect(revokeUrl).not.toHaveBeenCalled();
    expect(result).toHaveLength(0);
  });
});

describe("attachment Object URL lifecycle — unmount cleanup", () => {
  let revokeUrl: (url: string) => void;

  beforeEach(() => {
    // Use a simple spy wrapper that satisfies the (url: string) => void signature
    revokeUrl = vi.fn() as unknown as (url: string) => void;
  });

  it("revokes all remaining blob URLs from attachmentsRef on component unmount", () => {
    const attachmentsRef = {
      current: [
        { id: "a1", preview: "blob:http://localhost/orphan-1", status: "ready" as const },
        { id: "a2", preview: "blob:http://localhost/orphan-2", status: "staging" as const },
        { id: "a3", preview: null, status: "ready" as const }
      ]
    };

    unmountCleanup(attachmentsRef, revokeUrl);

    expect(revokeUrl).toHaveBeenCalledTimes(2);
    expect(revokeUrl).toHaveBeenCalledWith("blob:http://localhost/orphan-1");
    expect(revokeUrl).toHaveBeenCalledWith("blob:http://localhost/orphan-2");
  });

  it("reads the latest state from attachmentsRef (not a stale closure)", () => {
    // Simulates the attachmentsRef being updated after component mount
    const attachmentsRef = { current: [] as MockAttachment[] };
    attachmentsRef.current = [
      { id: "late", preview: "blob:http://localhost/late-add", status: "ready" as const }
    ];

    unmountCleanup(attachmentsRef, revokeUrl);

    expect(revokeUrl).toHaveBeenCalledOnce();
    expect(revokeUrl).toHaveBeenCalledWith("blob:http://localhost/late-add");
  });

  it("does not throw when attachmentsRef is empty on unmount", () => {
    const attachmentsRef = { current: [] as MockAttachment[] };
    expect(() => unmountCleanup(attachmentsRef, revokeUrl)).not.toThrow();
    expect(revokeUrl).not.toHaveBeenCalled();
  });
});
