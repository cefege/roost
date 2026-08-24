// Composes the standalone extension sources installed into user agent config
// dirs. The integration files import the shared report transport in-repo, but
// their deployed form must be a SINGLE self-contained module — nothing at
// ~/.omp or ~/.pi can resolve "@roost/*" or relative worker imports. Both
// deploy paths (from-source install and gen-embed's baked text) splice the
// transport source through here so there is exactly one transport definition.

const TRANSPORT_MARKER = /^import .*report-transport\.ts";$/m;

/** Replace an integration source's report-transport import with the transport
 *  module body itself. Throws if the marker is gone: that means someone
 *  refactored the import away and the composed asset would silently lose the
 *  reporter. */
export function composeStandaloneIntegration(
	integrationSource: string,
	transportSource: string,
): string {
	if (!TRANSPORT_MARKER.test(integrationSource)) {
		throw new Error("integration source lost its report-transport import marker");
	}
	const markerRemoved = integrationSource.replace(
		TRANSPORT_MARKER,
		"// (the shared report transport above replaces this import at embed/install time)",
	);
	return `${transportSource}\n\n${markerRemoved}`;
}
