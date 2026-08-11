export function shouldShowLegacyPmQueue(
  sessionSource: string,
  superAssistantEndpoint: boolean,
): boolean {
  return sessionSource === "pm" && !superAssistantEndpoint;
}
