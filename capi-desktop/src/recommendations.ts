export interface PlayEvent {
  id?: number;
  trackId: string;
  title: string;
  artist: string;
  album: string;
  thumbnail: string;
  timestamp: number;
  artistId?: string;
}

export interface TrackAffinity {
  trackId: string;
  playCount: number;
  lastPlayed: number;
  totalListenedMs: number;
  title: string;
  artist: string;
  artistId: string;
  thumbnail: string;
}

export interface ArtistAffinity {
  artistName: string;
  playCount: number;
  lastPlayed: number;
  artistId?: string;
  thumbnail?: string;
}

const DB_NAME = "capi_stats";
const DB_VERSION = 2;

function openDB(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);

    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains("play_events")) {
        db.createObjectStore("play_events", { keyPath: "id", autoIncrement: true });
      }
      if (!db.objectStoreNames.contains("artist_affinity")) {
        db.createObjectStore("artist_affinity", { keyPath: "artistName" });
      }
      if (!db.objectStoreNames.contains("track_affinity")) {
        const store = db.createObjectStore("track_affinity", { keyPath: "trackId" });
        store.createIndex("lastPlayed", "lastPlayed", { unique: false });
        store.createIndex("playCount", "playCount", { unique: false });
      }
      if (!db.objectStoreNames.contains("track_cache")) {
        const store = db.createObjectStore("track_cache", { keyPath: "id" });
        store.createIndex("artist", "artist", { unique: false });
      }
    };
  });
}

export async function recordPlayEvent(track: {
  id: string;
  title: string;
  artist: string;
  album?: string;
  thumbnail: string;
  artistId?: string;
}, listenedMs?: number): Promise<void> {
  try {
    const db = await openDB();
    const tx = db.transaction(["play_events", "artist_affinity", "track_affinity", "track_cache"], "readwrite");
    const playStore = tx.objectStore("play_events");
    const affinityStore = tx.objectStore("artist_affinity");
    const trackAffStore = tx.objectStore("track_affinity");
    const trackCacheStore = tx.objectStore("track_cache");

    const timestamp = Date.now();
    const event: PlayEvent = {
      trackId: track.id,
      title: track.title,
      artist: track.artist,
      album: track.album || "",
      thumbnail: track.thumbnail,
      timestamp,
      artistId: track.artistId
    };
    playStore.add(event);

    // Cache track metadata
    trackCacheStore.put({
      id: track.id,
      title: track.title,
      artist: track.artist,
      album: track.album || "",
      duration: 0,
      thumbnail: track.thumbnail,
      explicit: false,
      artistId: track.artistId,
    });

    // Update artist affinity
    const primaryArtist = track.artist.split(",")[0].trim();
    if (primaryArtist && primaryArtist !== "Artista Desconocido") {
      const getReq = affinityStore.get(primaryArtist);
      getReq.onsuccess = () => {
        const data: ArtistAffinity = getReq.result || {
          artistName: primaryArtist,
          playCount: 0,
          lastPlayed: 0
        };
        data.playCount += 1;
        data.lastPlayed = timestamp;
        if (track.artistId) {
          data.artistId = track.artistId.split(",")[0].trim();
        }
        if (track.thumbnail) {
          data.thumbnail = track.thumbnail;
        }
        affinityStore.put(data);
      };
    }

    // Update track affinity
    const getTrackReq = trackAffStore.get(track.id);
    getTrackReq.onsuccess = () => {
      const data: TrackAffinity = getTrackReq.result || {
        trackId: track.id,
        playCount: 0,
        lastPlayed: 0,
        totalListenedMs: 0,
        title: track.title,
        artist: track.artist,
        artistId: track.artistId || "",
        thumbnail: track.thumbnail,
      };
      data.playCount += 1;
      data.lastPlayed = timestamp;
      data.title = track.title;
      data.artist = track.artist;
      data.artistId = track.artistId || "";
      data.thumbnail = track.thumbnail;
      if (listenedMs !== undefined) {
        data.totalListenedMs += listenedMs;
      }
      trackAffStore.put(data);
    };
  } catch (err) {
    console.error("Failed to record play event in IndexedDB:", err);
  }
}

export async function addListenedTime(trackId: string, ms: number): Promise<void> {
  try {
    const db = await openDB();
    const tx = db.transaction("track_affinity", "readwrite");
    const store = tx.objectStore("track_affinity");
    const req = store.get(trackId);
    req.onsuccess = () => {
      const data: TrackAffinity | undefined = req.result;
      if (data) {
        data.totalListenedMs = (data.totalListenedMs || 0) + ms;
        store.put(data);
      }
    };
  } catch (err) {
    console.error("Failed to add listened time:", err);
  }
}

export async function getTopArtistsFromDB(limit = 10): Promise<ArtistAffinity[]> {
  try {
    const db = await openDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction("artist_affinity", "readonly");
      const store = tx.objectStore("artist_affinity");
      const request = store.getAll();

      request.onsuccess = () => {
        const list: ArtistAffinity[] = request.result || [];
        const now = Date.now();
        list.sort((a, b) => {
          const hoursA = (now - a.lastPlayed) / (1000 * 60 * 60);
          const hoursB = (now - b.lastPlayed) / (1000 * 60 * 60);
          const recencyA = Math.exp(-hoursA / 72);
          const recencyB = Math.exp(-hoursB / 72);
          const scoreA = a.playCount * (0.5 + 0.5 * recencyA);
          const scoreB = b.playCount * (0.5 + 0.5 * recencyB);
          return scoreB - scoreA;
        });
        resolve(list.slice(0, limit));
      };
      request.onerror = () => reject(request.error);
    });
  } catch (err) {
    console.error("Failed to get top artists:", err);
    return [];
  }
}

export async function getCachedTracksByArtist(artist: string, limit = 8): Promise<any[]> {
  try {
    const db = await openDB();
    return new Promise((resolve) => {
      const tx = db.transaction("track_cache", "readonly");
      const store = tx.objectStore("track_cache");
      const index = store.index("artist");
      const req = index.getAll(artist);
      req.onsuccess = () => {
        const results = (req.result || []).slice(0, limit);
        resolve(results);
      };
      req.onerror = () => resolve([]);
    });
  } catch {
    return [];
  }
}

function getTrackScore(track: TrackAffinity): number {
  const now = Date.now();
  const hoursSincePlayed = (now - track.lastPlayed) / (1000 * 60 * 60);
  const recencyWeight = Math.exp(-hoursSincePlayed / 72);
  const completionRatio = track.totalListenedMs > 0
    ? Math.min(1, track.totalListenedMs / (track.playCount * 60000))
    : 0.5;
  return track.playCount * (0.5 + 0.5 * recencyWeight) * (0.4 + 0.6 * completionRatio);
}

export async function getTopTracksFromDB(limit = 12): Promise<TrackAffinity[]> {
  try {
    const db = await openDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction("track_affinity", "readonly");
      const store = tx.objectStore("track_affinity");
      const request = store.getAll();

      request.onsuccess = () => {
        const list: TrackAffinity[] = request.result || [];
        list.sort((a, b) => getTrackScore(b) - getTrackScore(a));
        resolve(list.slice(0, limit));
      };
      request.onerror = () => reject(request.error);
    });
  } catch (err) {
    console.error("Failed to get top tracks:", err);
    return [];
  }
}

export async function getRecentTracksFromDB(limit = 12): Promise<PlayEvent[]> {
  try {
    const db = await openDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction("play_events", "readonly");
      const store = tx.objectStore("play_events");
      const request = store.getAll();

      request.onsuccess = () => {
        const events: PlayEvent[] = request.result || [];
        events.sort((a, b) => b.timestamp - a.timestamp);
        const seen = new Set<string>();
        const unique: PlayEvent[] = [];
        for (const e of events) {
          if (!seen.has(e.trackId)) {
            seen.add(e.trackId);
            unique.push(e);
            if (unique.length >= limit) break;
          }
        }
        resolve(unique);
      };
      request.onerror = () => reject(request.error);
    });
  } catch (err) {
    console.error("Failed to get recent tracks:", err);
    return [];
  }
}

export async function getRecommendationQueries(minTotalPlays = 20): Promise<string[]> {
  try {
    const allArtists = await getTopArtistsFromDB(10);
    if (allArtists.length === 0) return [];

    const totalPlays = allArtists.reduce((sum, item) => sum + item.playCount, 0);
    if (totalPlays < minTotalPlays) {
      console.log(`[Recs] Total plays (${totalPlays}) is below threshold (${minTotalPlays}). Using fallback recommendations.`);
      return [];
    }

    const now = Date.now();
    const scored = allArtists.map(item => {
      const hoursSincePlayed = (now - item.lastPlayed) / (1000 * 60 * 60);
      const recencyWeight = Math.exp(-hoursSincePlayed / 48);
      const score = item.playCount * (0.5 + 0.5 * recencyWeight);
      return { artistName: item.artistName, score };
    });

    scored.sort((a, b) => b.score - a.score);
    const top3 = scored.slice(0, 3).map(item => item.artistName);
    const queries: string[] = [];
    top3.forEach(artist => {
      queries.push(`${artist} mix`);
      queries.push(`${artist} radio`);
    });

    return queries;
  } catch (err) {
    console.error("Failed to generate recommendation queries:", err);
    return [];
  }
}

export interface HomeSection {
  id: string;
  title: string;
  type: "favorites" | "recent" | "artist_tracks" | "discover";
  items: any[];
}

export async function getPersonalizedHomeSections(_buscarCancion: (query: string) => Promise<any[]>): Promise<HomeSection[]> {
  const sections: HomeSection[] = [];

  try {
    const topTracks = await getTopTracksFromDB(10);
    if (topTracks.length >= 3) {
      sections.push({
        id: "favorites",
        title: "Tus favoritos",
        type: "favorites",
        items: topTracks.map(t => ({
          id: t.trackId,
          title: t.title,
          artists: [{ name: t.artist, id: t.artistId }],
          thumbnail: t.thumbnail,
          album: "",
          duration: 0,
          explicit: false,
        })),
      });
    }

    const recent = await getRecentTracksFromDB(10);
    if (recent.length >= 3) {
      sections.push({
        id: "recent",
        title: "Escuchados recientemente",
        type: "recent",
        items: recent.map(e => ({
          id: e.trackId,
          title: e.title,
          artists: [{ name: e.artist, id: e.artistId }],
          thumbnail: e.thumbnail,
          album: e.album,
          duration: 0,
          explicit: false,
        })),
      });
    }
  } catch (err) {
    console.error("Failed to generate personalized home sections:", err);
  }

  return sections;
}

export interface CapiSmartPlaylist {
  id: string;
  name: string;
  description: string;
  subtitle: string;
  coverGradient: string;
  coverImage?: string;
  badge?: string;
  tracks: any[];
}

export async function generateCapiPlaylists(
  buscarCancion: (query: string) => Promise<any[]>
): Promise<CapiSmartPlaylist[]> {
  const playlists: CapiSmartPlaylist[] = [];

  try {
    const topTracks = await getTopTracksFromDB(24);
    const topArtists = await getTopArtistsFromDB(8);
    const recentTracks = await getRecentTracksFromDB(15);

    // 1. "Mix Diario de Capi"
    const dailyTracks: any[] = [];
    const seenIds = new Set<string>();

    for (const t of topTracks.slice(0, 8)) {
      if (!seenIds.has(t.trackId)) {
        seenIds.add(t.trackId);
        dailyTracks.push({
          id: t.trackId,
          title: t.title,
          artist: t.artist,
          album: "",
          duration: 0,
          thumbnail: t.thumbnail,
          artistId: t.artistId,
        });
      }
    }

    for (const r of recentTracks.slice(0, 5)) {
      if (!seenIds.has(r.trackId)) {
        seenIds.add(r.trackId);
        dailyTracks.push({
          id: r.trackId,
          title: r.title,
          artist: r.artist,
          album: r.album || "",
          duration: 0,
          thumbnail: r.thumbnail,
          artistId: r.artistId,
        });
      }
    }

    // Top artist songs for daily mix
    for (const a of topArtists.slice(0, 2)) {
      try {
        const cached = await getCachedTracksByArtist(a.artistName, 4);
        for (const item of cached) {
          const tid = item.id || item.videoId;
          if (tid && !seenIds.has(tid)) {
            seenIds.add(tid);
            dailyTracks.push(item);
          }
        }
        if (dailyTracks.length < 12) {
          const queryTracks = await buscarCancion(`${a.artistName} mix`);
          for (const item of queryTracks.slice(0, 4)) {
            const tid = item.id || item.videoId;
            if (tid && !seenIds.has(tid)) {
              seenIds.add(tid);
              dailyTracks.push(item);
            }
          }
        }
      } catch {}
    }

    if (dailyTracks.length >= 3) {
      playlists.push({
        id: "capi-smart-daily-mix",
        name: "Mix Diario de Capi",
        description: "Tu mezcla personalizada de hoy con tus canciones más reproducidas y sugerencias afines.",
        subtitle: "Creado por Capi para ti",
        coverGradient: "linear-gradient(135deg, #6366f1 0%, #4338ca 50%, #1e1b4b 100%)",
        coverImage: dailyTracks[0]?.thumbnail,
        badge: "Mix Diario",
        tracks: dailyTracks,
      });
    }

    // 2. Artist Playlists: "Mix de [Artista]" para tus artistas más escuchados
    const artistGradients = [
      "linear-gradient(135deg, #059669 0%, #0d9488 50%, #064e3b 100%)",
      "linear-gradient(135deg, #e11d48 0%, #be123c 50%, #4c0519 100%)",
      "linear-gradient(135deg, #d97706 0%, #b45309 50%, #451a03 100%)",
      "linear-gradient(135deg, #8b5cf6 0%, #6d28d9 50%, #2e1065 100%)",
      "linear-gradient(135deg, #0284c7 0%, #0369a1 50%, #082f49 100%)",
    ];

    for (let i = 0; i < topArtists.slice(0, 4).length; i++) {
      const artist = topArtists[i];
      const artistTracks: any[] = [];
      const artistSeen = new Set<string>();

      // Canciones en historial del usuario
      for (const t of topTracks) {
        if (t.artist.toLowerCase().includes(artist.artistName.toLowerCase()) && !artistSeen.has(t.trackId)) {
          artistSeen.add(t.trackId);
          artistTracks.push({
            id: t.trackId,
            title: t.title,
            artist: t.artist,
            album: "",
            duration: 0,
            thumbnail: t.thumbnail,
            artistId: t.artistId || artist.artistId,
          });
        }
      }

      // Canciones más populares del artista
      try {
        const hits = await buscarCancion(`${artist.artistName} greatest hits`);
        for (const item of hits) {
          const tid = item.id || item.videoId;
          if (tid && !artistSeen.has(tid)) {
            artistSeen.add(tid);
            artistTracks.push(item);
          }
        }
      } catch {}

      if (artistTracks.length >= 3) {
        playlists.push({
          id: `capi-smart-artist-${encodeURIComponent(artist.artistName.toLowerCase().replace(/\s+/g, "-"))}`,
          name: `Mix de ${artist.artistName}`,
          description: `Las mejores canciones de ${artist.artistName} y música similar seleccionadas según lo que escuchas.`,
          subtitle: `Basado en ${artist.artistName}`,
          coverGradient: artistGradients[i % artistGradients.length],
          coverImage: artist.thumbnail || artistTracks[0]?.thumbnail,
          badge: `Artista • ${artist.artistName}`,
          tracks: artistTracks,
        });
      }
    }

    // 3. "Descubrimiento Capi" (artistas y canciones afines)
    if (topArtists.length > 0) {
      const discoveryTracks: any[] = [];
      const discSeen = new Set<string>();
      const existingUserIds = new Set(topTracks.map(t => t.trackId));

      try {
        const seed = topArtists[0].artistName;
        const discResults = await buscarCancion(`${seed} radio playlist`);
        for (const item of discResults) {
          const tid = item.id || item.videoId;
          if (tid && !discSeen.has(tid) && !existingUserIds.has(tid)) {
            discSeen.add(tid);
            discoveryTracks.push(item);
          }
        }
      } catch {}

      if (discoveryTracks.length >= 4) {
        playlists.push({
          id: "capi-smart-discovery",
          name: "Descubrimiento Capi",
          description: "Nuevas canciones y talentos similares a lo que más disfrutas escuchar.",
          subtitle: "Descubre nueva música",
          coverGradient: "linear-gradient(135deg, #06b6d4 0%, #0284c7 50%, #0c4a6e 100%)",
          coverImage: discoveryTracks[0]?.thumbnail,
          badge: "Descubrimiento",
          tracks: discoveryTracks,
        });
      }
    }

    // 4. "Tus Favoritos de Siempre"
    if (topTracks.length >= 4) {
      playlists.push({
        id: "capi-smart-favorites",
        name: "Tus Favoritos de Siempre",
        description: "Las canciones que más has repetido y escuchado en Capi.",
        subtitle: "Tus más escuchadas",
        coverGradient: "linear-gradient(135deg, #f43f5e 0%, #be123c 50%, #4c0519 100%)",
        coverImage: topTracks[0]?.thumbnail,
        badge: "En bucle",
        tracks: topTracks.map(t => ({
          id: t.trackId,
          title: t.title,
          artist: t.artist,
          album: "",
          duration: 0,
          thumbnail: t.thumbnail,
          artistId: t.artistId,
        })),
      });
    }

    // Fallback si no hay historial previo todavía
    if (playlists.length === 0) {
      try {
        const topHits = await buscarCancion("top canciones exitos 2024");
        if (topHits.length >= 4) {
          playlists.push({
            id: "capi-smart-welcome",
            name: "Mix de Bienvenida Capi",
            description: "Los mayores éxitos del momento seleccionados para empezar a disfrutar de tu música.",
            subtitle: "Creado por Capi para ti",
            coverGradient: "linear-gradient(135deg, #6366f1 0%, #4338ca 50%, #1e1b4b 100%)",
            coverImage: topHits[0]?.thumbnail,
            badge: "Bienvenida",
            tracks: topHits,
          });
        }
      } catch {}
    }
  } catch (err) {
    console.error("Error al generar playlists inteligentes de Capi:", err);
  }

  return playlists;
}
