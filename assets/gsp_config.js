export const config = {
    layer: {
        name: "/fire/detection/GSP",
        description: "Lightning Ground Strike Points (LoRa ingest)",
        show: true,
    },
    // publish threshold mirrors BR-F5-01: conf >= 0.5
    minConfidence: 50, // 0-100, filters display only
    followLatest: true,
    // --- sensor dot (always visible, one per registered sensor) ---
    sensorPointSize: 10,
    sensorColor: Cesium.Color.fromCssColorString('#0066FF'), // blue
    sensorOutline: Cesium.Color.fromCssColorString('#FFFFFF'),
    sensorOutlineWidth: 2,
    // --- strike dot (only for Confirmed, inside the red cone) ---
    pointSize: 8,
    outlineWidth: 2,
    strikeColor: Cesium.Color.fromCssColorString('#FF0000'), // red
    pointOutline: Cesium.Color.fromCssColorString('#FFFFFF'),
    unlocatedColor: Cesium.Color.fromCssColorString('Gray'),
    // --- FOV cone colors by state ---
    coneIdleColor: Cesium.Color.fromCssColorString('#808080'), // grey baseline
    coneCandidateColor: Cesium.Color.fromCssColorString('#FFFF00'), // yellow
    coneProbableColor: Cesium.Color.fromCssColorString('#FFA500'), // orange
    coneConfirmedColor: Cesium.Color.fromCssColorString('#FF0000'), // red
    coneAlpha: 0.3,
    // pixel_col -> azimuth mapping (assumed imager width until firmware confirms)
    sensorCols: 640,
    pointDC: new Cesium.DistanceDisplayCondition(0, Number.MAX_VALUE),
    zoomHeight: 50000,
};
