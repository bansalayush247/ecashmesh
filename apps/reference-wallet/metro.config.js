// The app consumes @ecashmesh/client from ../../packages through a file: link.
// Metro must watch that folder and resolve its peer dependencies (zod,
// nostr-tools) from this app's node_modules.
const path = require("node:path");
const { getDefaultConfig } = require("expo/metro-config");

const config = getDefaultConfig(__dirname);
config.watchFolders = [path.resolve(__dirname, "../../packages")];
config.resolver.nodeModulesPaths = [path.resolve(__dirname, "node_modules")];
module.exports = config;
