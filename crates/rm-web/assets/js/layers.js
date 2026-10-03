// Leaflet layer management: route path/stops, highlights, viewport segments.
import { METHOD_COLORS, routeCountColor } from './colors.js';

export const layers = {};

export function initLayers(map) {
  layers.routePath = L.layerGroup().addTo(map);
  layers.routeStops = L.layerGroup().addTo(map);
  layers.highlight = L.layerGroup().addTo(map);
  layers.segments = L.layerGroup().addTo(map);
  layers.stopsLayer = L.layerGroup().addTo(map);
  layers.osm = L.layerGroup().addTo(map);
  layers.raw = L.layerGroup().addTo(map);
}

export function clearRoute() {
  layers.routePath.clearLayers();
  layers.routeStops.clearLayers();
  layers.osm.clearLayers();
  layers.raw.clearLayers();
}

export function clearHighlight() {
  layers.highlight.clearLayers();
}

/// Draw canonical path pieces + stop markers + projected ghost markers.
export function drawRoute(pathFc, stopsJson, { onStopClick }) {
  clearRoute();
  L.geoJSON(pathFc, {
    style: (f) =>
      f.properties.geometry_method === 'no_path'
        ? { color: '#cf3e3e', dashArray: '6 6', weight: 3 }
        : { color: '#2563eb', weight: 4 },
  }).addTo(layers.routePath);

  for (const s of stopsJson.stops ?? []) {
    const m = L.circleMarker([s.lat, s.lon], {
      radius: 5,
      color: '#fff',
      weight: 1,
      fillColor: METHOD_COLORS[s.method] ?? '#7da7d9',
      fillOpacity: 1,
    });
    m.bindTooltip(`${s.stop_sequence}. ${s.stop_name}`, { direction: 'top' });
    m.on('click', () => onStopClick(s));
    m.addTo(layers.routeStops);

    if (s.projected && s.method === 'PROJECTED') {
      L.circleMarker([s.projected[1], s.projected[0]], {
        radius: 2,
        color: METHOD_COLORS.PROJECTED,
        weight: 1,
        fillOpacity: 0,
      }).addTo(layers.routeStops);
    }
  }
}

export function fitRoute(pathFc, stopsJson) {
  const pts = [];
  for (const f of pathFc.features ?? []) {
    for (const c of f.geometry?.coordinates ?? []) pts.push([c[1], c[0]]);
  }
  for (const s of stopsJson.stops ?? []) pts.push([s.lat, s.lon]);
  if (pts.length) layers.routePath._map.fitBounds(L.latLngBounds(pts).pad(0.05));
}

/// Ring group-member stations for stop drill-down.
export function highlightGroup(members) {
  clearHighlight();
  for (const m of members ?? []) {
    L.circleMarker([m.lat, m.lon], {
      radius: 12,
      color: '#d19a00',
      weight: 2,
      fillOpacity: 0.15,
    })
      .bindTooltip(`group: ${m.station_name}`, { direction: 'top' })
      .addTo(layers.highlight);
  }
}

export function flashStop(lat, lon) {
  clearHighlight();
  const m = L.circleMarker([lat, lon], {
    radius: 10,
    color: '#cf3e3e',
    weight: 3,
    fillOpacity: 0.2,
  }).addTo(layers.highlight);
  m.bindTooltip('outlier', { direction: 'top' });
}

/// Viewport-driven canonical-segment corridors, colored by route_count heat.
export function drawSegments(fc, { onSegmentClick }) {
  layers.segments.clearLayers();
  L.geoJSON(fc, {
    style: (f) => ({
      color: routeCountColor(f.properties.route_count ?? 1),
      weight: 2.5,
      opacity: 0.75,
    }),
  })
    .bindTooltip((l) => `${l.feature.properties.route_count} routes · ${Math.round(l.feature.properties.length_m)}m`)
    .on('click', (e) => onSegmentClick(e.layer.feature.properties.segment_id))
    .addTo(layers.segments);
}

export function clearSegments() {
  layers.segments.clearLayers();
}

/// Matched OSM ways overlay for the selected route+direction (dashed green).
export function drawWays(fc) {
  layers.osm.clearLayers();
  L.geoJSON(fc, {
    style: { color: '#2e7d32', weight: 3, dashArray: '4 6', opacity: 0.85 },
  })
    .bindTooltip((l) => `way ${l.feature.properties.osm_way_id} · ${l.feature.properties.name ?? ''}`)
    .addTo(layers.osm);
}

export function clearWays() {
  layers.osm.clearLayers();
}

/// Raw staging compare: dashed blue src_route_point + square raw stops.
export function drawRaw(raw) {
  layers.raw.clearLayers();
  if (raw?.path?.geometry) {
    L.geoJSON(raw.path, {
      style: { color: '#7c3aed', weight: 3, dashArray: '2 7', opacity: 0.8 },
    })
      .bindTooltip('src_route_point (raw)')
      .addTo(layers.raw);
  }
  for (const s of raw?.stops ?? []) {
    L.rectangle(
      [
        [s.lat - 0.00012, s.lon - 0.00012],
        [s.lat + 0.00012, s.lon + 0.00012],
      ],
      { color: '#7c3aed', weight: 1.5, fillOpacity: 0.35 }
    )
      .bindTooltip(`${s.source_order_no}. ${s.station_name} (node ${s.node_id})`, { direction: 'top' })
      .addTo(layers.raw);
  }
}

export function clearRaw() {
  layers.raw.clearLayers();
}

/// Viewport stops/stations: small dots + rings, zoom-gated by app.js.
export function drawStopsLayer(fc, { onStopClick }) {
  layers.stopsLayer.clearLayers();
  for (const f of fc.features ?? []) {
    const [lon, lat] = f.geometry.coordinates;
    const isStation = f.properties.kind === 'station';
    const m = isStation
      ? L.circleMarker([lat, lon], { radius: 7, color: '#d19a00', weight: 1.5, fillOpacity: 0.1 })
      : L.circleMarker([lat, lon], { radius: 2.5, color: '#404040', weight: 0.5, fillOpacity: 0.7 });
    m.bindTooltip(f.properties.name, { direction: 'top' });
    if (!isStation) m.on('click', () => onStopClick(f.properties.id));
    m.addTo(layers.stopsLayer);
  }
}

export function clearStopsLayer() {
  layers.stopsLayer.clearLayers();
}
