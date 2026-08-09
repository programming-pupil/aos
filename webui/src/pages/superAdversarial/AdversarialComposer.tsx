import { Alert, Button, Form, Input, InputNumber, Select, Space, Tooltip } from 'antd';
import { SendOutlined } from '@ant-design/icons';
import type { FormInstance } from 'antd';
import type { TFunction } from 'i18next';
import type { ChatAdversarialRun } from '@/types';
import {
  ADVERSARIAL_DEFAULT_MAX_ROUNDS,
  ADVERSARIAL_HARD_MAX_ROUNDS,
} from './utils';

type ComposerFormValues = {
  question: string;
  models: string[];
  max_rounds: number;
};

type Props = {
  t: TFunction;
  form: FormInstance<ComposerFormValues>;
  modelOptions: Array<{ value: string; label: string }>;
  followupParentRun: ChatAdversarialRun | null;
  canSubmit: boolean;
  submitting: boolean;
  onSubmit: () => void;
  onNewRun: () => void;
  onTooManyModels: () => void;
};

export function AdversarialComposer({
  t,
  form,
  modelOptions,
  followupParentRun,
  canSubmit,
  submitting,
  onSubmit,
  onNewRun,
  onTooManyModels,
}: Props) {
  return (
    <footer className="super-adversarial__composer">
      {followupParentRun ? (
        <Alert
          type="info"
          showIcon
          message={t('chat.adversarialFollowupModeHint', {
            iteration: followupParentRun.iteration_no,
          })}
          action={
            <Button size="small" onClick={onNewRun}>
              {t('chat.adversarialNewSession')}
            </Button>
          }
          className="super-adversarial__followup-alert"
        />
      ) : null}
      {modelOptions.length < 2 ? (
        <Alert type="warning" showIcon message={t('chat.adversarialNeedModels')} />
      ) : (
        <Form form={form} layout="vertical" initialValues={{ max_rounds: ADVERSARIAL_DEFAULT_MAX_ROUNDS }}>
          <div className="super-adversarial__composer-grid">
            <Form.Item
              name="models"
              label={t('chat.adversarialModels')}
              rules={[
                {
                  validator: (_, value: string[] | undefined) => {
                    const count = value?.length ?? 0;
                    if (count >= 2 && count <= 3) return Promise.resolve();
                    return Promise.reject(new Error(t('chat.adversarialModelRule')));
                  },
                },
              ]}
              className="super-adversarial__form-item"
            >
              <Select
                mode="multiple"
                options={modelOptions}
                placeholder={t('chat.adversarialModelsPlaceholder')}
                onChange={(value) => {
                  if (value.length > 3) {
                    form.setFieldValue('models', value.slice(0, 3));
                    onTooManyModels();
                  }
                }}
              />
            </Form.Item>
            <Form.Item
              name="max_rounds"
              label={
                <Tooltip title={t('chat.adversarialMaxRoundsHelp')}>
                  <span>{t('chat.adversarialMaxRounds')}</span>
                </Tooltip>
              }
              rules={[{ required: true }]}
              className="super-adversarial__form-item"
            >
              <InputNumber min={1} max={ADVERSARIAL_HARD_MAX_ROUNDS} className="super-adversarial__rounds-input" />
            </Form.Item>
            <Space align="end" className="super-adversarial__send-cell">
              <Tooltip title={t('chat.adversarialHelp')}>
                <Button
                  type="primary"
                  size="large"
                  icon={<SendOutlined />}
                  loading={submitting}
                  disabled={!canSubmit || submitting}
                  onClick={onSubmit}
                >
                  {followupParentRun ? t('chat.adversarialSendFollowup') : t('chat.adversarialStartAction')}
                </Button>
              </Tooltip>
            </Space>
          </div>
          <Form.Item
            name="question"
            rules={[
              {
                required: true,
                whitespace: true,
                message: t('chat.adversarialQuestionRequired'),
              },
            ]}
            className="super-adversarial__question-item"
          >
            <Input.TextArea
              rows={3}
              placeholder={
                followupParentRun
                  ? t('chat.adversarialFollowupPlaceholder')
                  : t('chat.adversarialAskPlaceholder')
              }
              maxLength={8000}
              showCount
            />
          </Form.Item>
        </Form>
      )}
    </footer>
  );
}
