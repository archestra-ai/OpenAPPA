type YouTubeEmbedProps = {
  title: string;
  videoId: string;
};

export function YouTubeEmbed({ title, videoId }: YouTubeEmbedProps) {
  return (
    <div className="my-8 overflow-hidden rounded-xl border border-[var(--border)] bg-black shadow-sm">
      <iframe
        className="aspect-video w-full"
        src={`https://www.youtube-nocookie.com/embed/${videoId}`}
        title={title}
        loading="lazy"
        referrerPolicy="strict-origin-when-cross-origin"
        allow="accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture; web-share"
        allowFullScreen
      />
    </div>
  );
}

type VideoTile = YouTubeEmbedProps & { hint: string };

export function YouTubeTiles({ videos }: { videos: VideoTile[] }) {
  return (
    <div className="not-prose my-8 grid grid-cols-2 gap-3 sm:gap-4">
      {videos.map((video) => (
        <a
          key={video.videoId}
          href={`https://www.youtube.com/watch?v=${video.videoId}`}
          target="_blank"
          rel="noopener noreferrer"
          aria-label={`${video.title} (${video.hint}, opens YouTube in a new tab)`}
          className="group relative block aspect-video overflow-hidden rounded-xl border border-[var(--border)] bg-black shadow-sm"
        >
          <img
            src={`https://i.ytimg.com/vi/${video.videoId}/hqdefault.jpg`}
            alt=""
            loading="lazy"
            className="absolute inset-0 h-full w-full object-cover transition-opacity duration-200 group-hover:opacity-40 group-focus-visible:opacity-40"
          />
          <span className="absolute inset-0 flex items-center justify-center transition-opacity duration-200 group-hover:opacity-0 group-focus-visible:opacity-0">
            <svg viewBox="0 0 68 48" className="h-8 w-11 sm:h-10 sm:w-14" aria-hidden="true">
              <path d="M66.5 7.7a8.5 8.5 0 0 0-6-6C55.2.3 34 .3 34 .3s-21.2 0-26.5 1.4a8.5 8.5 0 0 0-6 6C.1 13 .1 24 .1 24s0 11 1.4 16.3a8.5 8.5 0 0 0 6 6c5.3 1.4 26.5 1.4 26.5 1.4s21.2 0 26.5-1.4a8.5 8.5 0 0 0 6-6C67.9 35 67.9 24 67.9 24s0-11-1.4-16.3Z" fill="#f00" />
              <path d="M45 24 27 14v20" fill="#fff" />
            </svg>
          </span>
          <span className="absolute inset-0 flex items-center justify-center px-3 text-center text-sm font-semibold text-white opacity-0 transition-opacity duration-200 group-hover:opacity-100 group-focus-visible:opacity-100 sm:text-base">
            {video.hint}
          </span>
        </a>
      ))}
    </div>
  );
}
