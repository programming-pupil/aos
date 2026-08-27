import { describe, expect, it } from 'vitest';
import { attachSuperAssistantTurnMetadata } from '../ChatCore';
import type { DisplayMessage } from '../chatCore.types';

describe('super assistant history metadata', () => {
  it('associates duplicate answers by the persisted user message', () => {
    const messages: DisplayMessage[] = [
      {
        id: 'user-automation',
        role: 'user',
        content: '每分钟生成 hello 文件',
      },
      {
        id: 'assistant-automation',
        role: 'assistant',
        content: '相同答案',
      },
      {
        id: 'user-adversarial',
        role: 'user',
        content: '北京海淀天气预报',
      },
      {
        id: 'assistant-adversarial',
        role: 'assistant',
        content: '相同答案',
      },
    ];

    const restored = attachSuperAssistantTurnMetadata(messages, [
      {
        turn_id: 'automation-turn',
        model: 'model-a',
        user_message: '每分钟生成 hello 文件',
        final_text: '相同答案',
        route_capability: 'pm_assistant',
        adversarial_run_id: null,
      },
      {
        turn_id: 'adversarial-turn',
        model: 'model-b',
        user_message: '/超级对抗 北京海淀天气预报',
        final_text: '相同答案',
        route_capability: 'super_adversarial',
        adversarial_run_id: 'chat-adv-1',
      },
    ]);

    expect(restored[1].adversarialRunId).toBeUndefined();
    expect(restored[3]).toMatchObject({ adversarialRunId: 'chat-adv-1' });
    expect(restored[2].content).toBe('/超级对抗 北京海淀天气预报');
    expect(restored[0].content).toBe('每分钟生成 hello 文件');
  });

  it('does not attach an adversarial turn when its user message is absent', () => {
    const messages: DisplayMessage[] = [
      { id: 'user-1', role: 'user', content: '普通问题' },
      { id: 'assistant-1', role: 'assistant', content: '答案' },
    ];
    const restored = attachSuperAssistantTurnMetadata(messages, [
      {
        turn_id: 'adversarial-turn',
        model: 'model-a',
        user_message: '/超级对抗 另一个问题',
        final_text: '答案',
        route_capability: 'super_adversarial',
        adversarial_run_id: 'chat-adv-2',
      },
    ]);

    expect(restored[1].adversarialRunId).toBeUndefined();
    expect(restored[0].content).toBe('普通问题');
  });

  it('restores the adversarial prefix for legacy rows that stored only the prompt', () => {
    const restored = attachSuperAssistantTurnMetadata(
      [
        { id: 'user-1', role: 'user', content: '北京海淀天气预报' },
        { id: 'assistant-1', role: 'assistant', content: '答案' },
      ],
      [
        {
          turn_id: 'adversarial-turn',
          model: 'model-a',
          user_message: '北京海淀天气预报',
          final_text: '答案',
          route_capability: 'super_adversarial',
          adversarial_run_id: 'chat-adv-3',
        },
      ],
    );

    expect(restored[0].content).toBe('/超级对抗 北京海淀天气预报');
    expect(restored[1].adversarialRunId).toBe('chat-adv-3');
  });
});
