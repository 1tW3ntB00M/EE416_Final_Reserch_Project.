export const config = {
    layer: {
        name: "/fire/detection/GSP",
        description: "Lightning Ground Strike Points (LoRa ingest)",
        show: true,
    },
    // publish threshold mirrors BR-F5-01: conf >= 0.5
    minConfidence: 50, // 0-100, filters display only
    followLatest: true,
    pointSize: 8,
    outlineWidth: 2,
    // color by confidence
    highConfColor: Cesium.Color.fromCssColorString('#FFFF00'),
    lowConfColor: Cesium.Color.fromCssColorString('#808080'),
    pointOutline: Cesium.Color.fromCssColorString('Yellow'),
    unlocatedColor: Cesium.Color.fromCssColorString('Gray'),
    pointDC: new Cesium.DistanceDisplayCondition(0, Number.MAX_VALUE),
    zoomHeight: 50000,
};
