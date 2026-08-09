import { Empty, Space, Tag, Tree, Typography } from 'antd';
import { FileTextOutlined, FolderOpenOutlined } from '@ant-design/icons';
import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import type { DataNode } from 'antd/es/tree';
import type { RdFileNode, RdTaskWorkbenchResponse } from '@/types';

const { Text } = Typography;

function nodeTitle(node: RdFileNode) {
  const tags = [];
  if (node.pendingCount) tags.push(<Tag key="pending" color="warning">{node.pendingCount}</Tag>);
  if (node.language) tags.push(<Tag key="lang">{node.language}</Tag>);
  return (
    <Space size={6} style={{ minWidth: 0 }}>
      {node.nodeType === 'file' ? <FileTextOutlined /> : <FolderOpenOutlined />}
      <Text ellipsis={{ tooltip: node.path }} style={{ maxWidth: 190 }}>{node.name}</Text>
      {tags}
    </Space>
  );
}

function toTreeData(nodes: RdFileNode[] = [], depth = 0): DataNode[] {
  return nodes.slice(0, depth === 0 ? 80 : 160).map((node) => ({
    key: node.path || node.name,
    title: nodeTitle(node),
    isLeaf: node.nodeType === 'file',
    children: node.children?.length ? toTreeData(node.children, depth + 1) : undefined,
  }));
}

export function WorkspaceSidebar({
  workbench,
  onSelectFile,
}: {
  workbench?: RdTaskWorkbenchResponse | null;
  onSelectFile?: (path: string) => void;
}) {
  const { t } = useTranslation();
  const treeData = useMemo(() => toTreeData(workbench?.fileTree ?? []), [workbench?.fileTree]);
  const changedGroups = workbench?.changedFileGroups ?? [];

  return (
    <div className="rd-workspace-files-panel">
      <Space direction="vertical" size={12} style={{ width: '100%', minWidth: 0 }}>
        <div>
          <Space style={{ justifyContent: 'space-between', width: '100%' }}>
            <Text strong>{t('rd.workspaceFiles', '文件树')}</Text>
            {workbench?.fileTree?.length ? <Tag>{workbench.fileTree.length}</Tag> : null}
          </Space>
          {treeData.length === 0 ? (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.workspaceFilesEmpty', '暂无文件树')} />
          ) : (
            <Tree
              className="rd-workspace-file-tree"
              treeData={treeData}
              height={260}
              defaultExpandAll={false}
              onSelect={(keys) => {
                const key = String(keys[0] ?? '');
                if (key) onSelectFile?.(key);
              }}
            />
          )}
        </div>

        <div>
          <Space style={{ justifyContent: 'space-between', width: '100%' }}>
            <Text strong>{t('rd.changedFiles', '变更文件')}</Text>
            {workbench?.fileChanges?.length ? <Tag color="blue">{workbench.fileChanges.length}</Tag> : null}
          </Space>
          {changedGroups.length === 0 ? (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.changedFilesEmpty', '暂无变更文件')} />
          ) : (
            <Space direction="vertical" size={8} style={{ width: '100%', marginTop: 8 }}>
              {changedGroups.map((group) => (
                <div key={group.changeType} className="rd-workspace-change-group">
                  <Space wrap>
                    <Tag>{group.changeType}</Tag>
                    <Tag color={group.pendingCount > 0 ? 'warning' : 'success'}>
                      {t('rd.pendingApplyCount', '{{count}} 个待应用', { count: group.pendingCount })}
                    </Tag>
                  </Space>
                  {group.files.slice(0, 8).map((file) => (
                    <button
                      type="button"
                      key={file.id}
                      className="rd-workspace-file-button"
                      onClick={() => onSelectFile?.(file.filePath)}
                    >
                      <span>{file.filePath}</span>
                      {file.applied ? <Tag color="success">{t('rd.applied', '已应用')}</Tag> : null}
                    </button>
                  ))}
                </div>
              ))}
            </Space>
          )}
        </div>
      </Space>
    </div>
  );
}
