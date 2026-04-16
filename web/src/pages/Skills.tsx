import { useEffect, useMemo, useState } from 'react';
import {
  Sparkles,
  Search,
  ChevronDown,
  ChevronRight,
  Package,
  Tag,
  User,
} from 'lucide-react';
import type { Skill } from '@/types/api';
import { getSkills } from '@/lib/api';
import { t } from '@/lib/i18n';

export default function Skills() {
  const [skills, setSkills] = useState<Skill[]>([]);
  const [search, setSearch] = useState('');
  const [expandedSkill, setExpandedSkill] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getSkills()
      .then(setSkills)
      .catch((err) => setError(err.message ?? String(err)))
      .finally(() => setLoading(false));
  }, []);

  const filtered = useMemo(
    () =>
      skills.filter((s) => {
        const q = search.toLowerCase();
        if (!q) return true;
        return (
          s.name.toLowerCase().includes(q) ||
          s.description.toLowerCase().includes(q) ||
          (s.author ?? '').toLowerCase().includes(q) ||
          s.tags.some((tag) => tag.toLowerCase().includes(q))
        );
      }),
    [skills, search],
  );

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div
          className="rounded-2xl border p-4"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {t('skills.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
        />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center justify-between gap-4 flex-wrap">
        <div className="flex items-center gap-2">
          <Sparkles className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h1 className="text-lg font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
            {t('skills.title')}
          </h1>
        </div>
        <p className="text-xs uppercase tracking-wider" style={{ color: 'var(--pc-text-muted)' }}>
          {t('skills.count')}:{' '}
          <span className="font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
            {skills.length}
          </span>
        </p>
      </div>

      {/* Search */}
      <div className="relative max-w-md">
        <Search
          className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4"
          style={{ color: 'var(--pc-text-faint)' }}
        />
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t('skills.search')}
          className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
        />
      </div>

      {filtered.length === 0 ? (
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          {t('skills.empty')}
        </p>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
          {filtered.map((skill) => {
            const isExpanded = expandedSkill === skill.name;
            const toolCount = skill.tools.length;
            const severityLabel = skill.max_severity || 'safe';
            const severityColor =
              severityLabel === 'critical'
                ? 'var(--color-status-error)'
                : severityLabel === 'warning'
                  ? 'var(--color-status-warn)'
                  : 'var(--color-status-success)';

            return (
              <div key={skill.name} className="card overflow-hidden animate-slide-in-up">
                <button
                  onClick={() => setExpandedSkill(isExpanded ? null : skill.name)}
                  className="w-full text-left p-4 transition-all"
                  style={{ background: 'transparent' }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.background = 'var(--pc-hover)';
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.background = 'transparent';
                  }}
                >
                  <div className="flex items-start justify-between gap-2">
                    <div className="flex flex-col gap-1 min-w-0">
                      <div className="flex items-center gap-2 min-w-0">
                        <Sparkles
                          className="h-4 w-4 flex-shrink-0"
                          style={{ color: 'var(--pc-accent)' }}
                        />
                        <h3
                          className="text-sm font-semibold truncate"
                          style={{ color: 'var(--pc-text-primary)' }}
                        >
                          {skill.name}
                        </h3>
                      </div>
                      <p
                        className="text-xs line-clamp-2"
                        style={{ color: 'var(--pc-text-muted)' }}
                      >
                        {skill.description}
                      </p>
                      <div className="flex flex-wrap items-center gap-2 mt-1">
                        <span
                          className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] border"
                          style={{
                            borderColor: severityColor,
                            color: severityColor,
                            background: 'transparent',
                          }}
                        >
                          {severityLabel}
                        </span>
                        {skill.author && (
                          <span
                            className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] border"
                            style={{
                              borderColor: 'var(--pc-border)',
                              color: 'var(--pc-text-secondary)',
                              background: 'var(--pc-bg-base)',
                            }}
                          >
                            <User className="h-3 w-3" />
                            {skill.author}
                          </span>
                        )}
                        <span
                          className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] border"
                          style={{
                            borderColor: 'var(--pc-border)',
                            color: 'var(--pc-text-secondary)',
                            background: 'var(--pc-bg-base)',
                          }}
                        >
                          <Package className="h-3 w-3" />
                          v{skill.version}
                        </span>
                        {toolCount > 0 && (
                          <span
                            className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] border"
                            style={{
                              borderColor: 'var(--pc-border)',
                              color: 'var(--pc-text-secondary)',
                              background: 'var(--pc-bg-base)',
                            }}
                          >
                            <WrenchMini />
                            {toolCount}
                          </span>
                        )}
                      </div>
                    </div>
                    {isExpanded ? (
                      <ChevronDown
                        className="h-4 w-4 flex-shrink-0"
                        style={{ color: 'var(--pc-accent)' }}
                      />
                    ) : (
                      <ChevronRight
                        className="h-4 w-4 flex-shrink-0"
                        style={{ color: 'var(--pc-text-faint)' }}
                      />
                    )}
                  </div>
                </button>

                {isExpanded && (
                  <div
                    className="border-t p-4 space-y-3 animate-fade-in"
                    style={{ borderColor: 'var(--pc-border)' }}
                  >
                    {skill.tags.length > 0 && (
                      <div className="flex flex-wrap gap-1.5">
                        {skill.tags.map((tag) => (
                          <span
                            key={tag}
                            className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] border"
                            style={{
                              borderColor: 'var(--pc-border)',
                              color: 'var(--pc-text-secondary)',
                              background: 'var(--pc-accent-glow)',
                            }}
                          >
                            <Tag className="h-3 w-3" />
                            {tag}
                          </span>
                        ))}
                      </div>
                    )}

                    {skill.tools.length > 0 && (
                      <div className="space-y-1">
                        <p
                          className="text-[10px] font-semibold uppercase tracking-wider"
                          style={{ color: 'var(--pc-text-muted)' }}
                        >
                          {t('skills.tools')}
                        </p>
                        <div className="space-y-2 max-h-64 overflow-y-auto pr-1">
                          {skill.tools.map((tool) => (
                            <div
                              key={tool.name}
                              className="rounded-xl px-3 py-2 border"
                              style={{
                                borderColor: 'var(--pc-border)',
                                background: 'var(--pc-bg-base)',
                              }}
                            >
                              <div className="flex items-center justify-between gap-2">
                                <div className="flex items-center gap-2 min-w-0">
                                  <Package
                                    className="h-3.5 w-3.5 flex-shrink-0"
                                    style={{ color: 'var(--pc-accent)' }}
                                  />
                                  <span
                                    className="text-xs font-medium truncate"
                                    style={{ color: 'var(--pc-text-primary)' }}
                                  >
                                    {tool.name}
                                  </span>
                                </div>
                                <span
                                  className="text-[10px] px-1.5 py-0.5 rounded-full border capitalize"
                                  style={{
                                    borderColor: 'var(--pc-border)',
                                    color: 'var(--pc-text-secondary)',
                                    background: 'var(--pc-bg-base)',
                                  }}
                                >
                                  {tool.kind}
                                </span>
                              </div>
                              {tool.description && (
                                <p
                                  className="text-xs mt-1 line-clamp-2"
                                  style={{ color: 'var(--pc-text-muted)' }}
                                >
                                  {tool.description}
                                </p>
                              )}
                              {tool.command && (
                                <pre
                                  className="mt-2 text-[10px] rounded-lg px-2 py-1 overflow-x-auto font-mono"
                                  style={{
                                    background: 'var(--pc-bg-elevated)',
                                    color: 'var(--pc-text-secondary)',
                                  }}
                                >
                                  {tool.command}
                                </pre>
                              )}
                            </div>
                          ))}
                        </div>
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function WrenchMini() {
  return (
    <span className="inline-flex items-center justify-center">
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        width={12}
        height={12}
        aria-hidden="true"
        className="text-(--pc-accent)"
      >
        <path
          d="M14.7 5.3a1 1 0 0 1 1.4 0l2.6 2.6a1 1 0 0 1 0 1.4L16 11.4l.6.6a2.5 2.5 0 0 1 0 3.5l-1.9 1.9a2.5 2.5 0 0 1-3.5 0l-1-1-3.3 3.3a1 1 0 0 1-1.4-1.4l3.3-3.3-1-1a2.5 2.5 0 0 1 0-3.5l1.9-1.9a2.5 2.5 0 0 1 3.5 0l.6.6 2.1-2.1a1 1 0 0 1 .4-.3Z"
          fill="currentColor"
        />
      </svg>
    </span>
  );
}

