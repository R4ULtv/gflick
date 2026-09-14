/**
 * Shared links. Everything else lives in the component that renders it - these
 * are here because REPO is used in five places and the doc URLs derive from it,
 * so a rename is one edit rather than a sweep.
 */
export const REPO = "https://github.com/R4ULtv/gflick";
export const RELEASES = `${REPO}/releases/latest`;
export const BENCH_DOC = `${REPO}/blob/main/apps/bench/README.md`;
export const MATRIX_DOC = `${REPO}/blob/main/docs/feature-matrix.md`;
export const SETUP_DOC = `${REPO}/blob/main/apps/setup/README.md`;

/**
 * The shipped version. Bump this by hand alongside the application crate
 * versions when cutting a release - .github/workflows/release.yml refuses to
 * publish unless this literal matches the git tag, so the two cannot drift.
 *
 * Archive names are version-stamped by scripts/package-release.*, so a direct
 * asset URL has to name the version; there is no `/releases/latest/download/`
 * shortcut while the filename itself carries the tag.
 */
export const VERSION = "0.3.0";

const ASSETS = `${REPO}/releases/download/v${VERSION}`;

/** Filenames must match scripts/package-release.* exactly or these 404. */
export const ARCHIVES = {
  windows: `gflick-${VERSION}-windows-x86_64.zip`,
  macos: `gflick-${VERSION}-macos-aarch64.tar.gz`,
};

export const DOWNLOADS = {
  windows: `${ASSETS}/${ARCHIVES.windows}`,
  macos: `${ASSETS}/${ARCHIVES.macos}`,
};

/** Hashes to check an archive against, published beside the archives. */
export const SHA256SUMS = `${ASSETS}/SHA256SUMS`;
