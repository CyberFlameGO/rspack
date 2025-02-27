/** @type {import("@rspack/core").Configuration} */
module.exports = {
	entry: {
		constructor: "./index"
	},
	target: "web",
	output: {
		filename: "[name].js"
	},
	optimization: {
		runtimeChunk: "single"
	}
};
